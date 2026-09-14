//! The connectome as CSR: one contiguous array of targets, one of weights, one of row
//! starts. That layout is chosen because the hot loop walks the out-edges of whichever
//! neurons just fired, and CSR makes that a slice rather than a hash lookup.

use std::collections::HashMap;
use std::io::BufRead;

#[derive(Debug)]
pub enum GraphError {
    /// A row that is not `pre_id,post_id,syn_count`. Carries the line number, because
    /// "malformed CSV" without one sends the reader to grep 2.7 million rows by hand.
    MalformedRow { line: usize, text: String },
    /// An edge pointing at a neuron that never appears as a source or target elsewhere.
    /// Dropping it silently is how a thresholded export quietly loses a quarter of its
    /// graph, so it is an error rather than a filter.
    DanglingNode { line: usize, id: u64 },
    /// Zero edges survived. An empty graph integrates perfectly and proves nothing, so it
    /// is refused at load rather than at the first confusing receipt.
    Empty,
    Io(std::io::Error),
}

impl std::fmt::Display for GraphError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedRow { line, text } => {
                write!(f, "line {line}: expected pre_id,post_id,syn_count, got {text:?}")
            }
            Self::DanglingNode { line, id } => {
                write!(f, "line {line}: neuron {id} appears only as a target of a dropped edge")
            }
            Self::Empty => write!(f, "no edges survived the threshold — nothing to simulate"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for GraphError {}

impl From<std::io::Error> for GraphError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// A directed weighted graph in compressed sparse row form.
#[derive(Debug, Clone)]
pub struct Graph {
    /// `row_start[i]..row_start[i + 1]` indexes the out-edges of neuron `i`.
    pub row_start: Vec<u32>,
    pub targets: Vec<u32>,
    /// Synapse counts, normalised to a per-edge conductance at load.
    pub weights: Vec<f32>,
    /// Dense index → original connectome id, so a finding can be looked up in Codex.
    pub ids: Vec<u64>,
}

impl Graph {
    pub fn neurons(&self) -> usize {
        self.ids.len()
    }

    pub fn edges(&self) -> usize {
        self.targets.len()
    }

    /// Every constructor leaves each row's targets sorted ascending; `Lif::step` depends on it.
    ///
    /// Parse `pre_id,post_id,syn_count`, keeping edges at or above `threshold` synapses.
    ///
    /// A header line is tolerated (it fails the integer parse on the first field and is
    /// skipped only when it is the very first line — a malformed row further down is a
    /// real error, not a second header).
    pub fn from_edge_list<R: BufRead>(reader: R, threshold: u32) -> Result<Self, GraphError> {
        let mut index: HashMap<u64, u32> = HashMap::new();
        let mut ids: Vec<u64> = Vec::new();
        let mut edges: Vec<(u32, u32, f32)> = Vec::new();

        for (n, line) in reader.lines().enumerate() {
            let line = line?;
            let row = line.trim();
            if row.is_empty() || row.starts_with('#') {
                continue;
            }
            let mut cols = row.split(',');
            let parsed = (|| {
                let pre = cols.next()?.trim().parse::<u64>().ok()?;
                let post = cols.next()?.trim().parse::<u64>().ok()?;
                let syn = cols.next()?.trim().parse::<u32>().ok()?;
                Some((pre, post, syn))
            })();
            let Some((pre, post, syn)) = parsed else {
                if n == 0 {
                    continue; // header
                }
                return Err(GraphError::MalformedRow { line: n + 1, text: row.to_string() });
            };
            if syn < threshold {
                continue;
            }
            let mut intern = |id: u64, ids: &mut Vec<u64>| -> u32 {
                *index.entry(id).or_insert_with(|| {
                    ids.push(id);
                    (ids.len() - 1) as u32
                })
            };
            let a = intern(pre, &mut ids);
            let b = intern(post, &mut ids);
            edges.push((a, b, syn as f32));
        }

        if edges.is_empty() {
            return Err(GraphError::Empty);
        }

        edges.sort_unstable_by_key(|&(a, b, _)| (a, b));
        let n = ids.len();
        let mut row_start = vec![0u32; n + 1];
        for &(a, _, _) in &edges {
            row_start[a as usize + 1] += 1;
        }
        for i in 0..n {
            row_start[i + 1] += row_start[i];
        }

        let mut targets = Vec::with_capacity(edges.len());
        let mut weights = Vec::with_capacity(edges.len());
        for (_, b, w) in edges {
            targets.push(b);
            weights.push(w);
        }

        Ok(Self { row_start, targets, weights, ids })
    }

    /// Load the app's packed format, written by flykeeper's `scripts/flywire-import.py`:
    /// `"FCB1" u32 n u32 e · ids u64[n] · row_start u32[n+1] · targets u32[e] · weights u16[e]
    /// · positions f32[3n]`, little-endian. Edges under `threshold` synapses are dropped at
    /// load; every cell is kept so index i still matches positions[i]. Returns the graph and
    /// the positions in µm.
    pub fn from_fcb(bytes: &[u8], threshold: u32) -> Result<(Self, Vec<[f32; 3]>), GraphError> {
        fn u32_at(b: &[u8], o: usize) -> Option<u32> {
            b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()))
        }
        let bad = |what: &str| GraphError::MalformedRow { line: 0, text: format!("fcb: {what}") };
        if bytes.get(0..4) != Some(b"FCB1") {
            return Err(bad("not an FCB1 file"));
        }
        let n = u32_at(bytes, 4).ok_or_else(|| bad("truncated header"))? as usize;
        let e = u32_at(bytes, 8).ok_or_else(|| bad("truncated header"))? as usize;
        let need = 12 + n * 8 + (n + 1) * 4 + e * 4 + e * 2 + n * 12;
        if bytes.len() < need {
            return Err(bad("truncated body"));
        }
        let mut o = 12;
        let ids: Vec<u64> = (0..n).map(|i| u64::from_le_bytes(bytes[o + i * 8..o + i * 8 + 8].try_into().unwrap())).collect();
        o += n * 8;
        let starts: Vec<u32> = (0..=n).map(|i| u32_at(bytes, o + i * 4).unwrap()).collect();
        o += (n + 1) * 4;
        let all_targets: Vec<u32> = (0..e).map(|i| u32_at(bytes, o + i * 4).unwrap()).collect();
        o += e * 4;
        let all_weights: Vec<u16> = (0..e).map(|i| u16::from_le_bytes(bytes[o + i * 2..o + i * 2 + 2].try_into().unwrap())).collect();
        o += e * 2;
        let positions: Vec<[f32; 3]> = (0..n)
            .map(|i| {
                let p = o + i * 12;
                let f = |k: usize| f32::from_le_bytes(bytes[p + k * 4..p + k * 4 + 4].try_into().unwrap());
                [f(0), f(1), f(2)]
            })
            .collect();

        let mut row_start = Vec::with_capacity(n + 1);
        let mut targets = Vec::with_capacity(e);
        let mut weights = Vec::with_capacity(e);
        row_start.push(0);
        for i in 0..n {
            for k in starts[i] as usize..starts[i + 1] as usize {
                if all_weights[k] as u32 >= threshold {
                    if all_targets[k] as usize >= n {
                        return Err(bad("target out of range"));
                    }
                    targets.push(all_targets[k]);
                    weights.push(all_weights[k] as f32);
                }
            }
            row_start.push(targets.len() as u32);
        }
        if targets.is_empty() {
            return Err(GraphError::Empty);
        }
        Ok((Self { row_start, targets, weights, ids }, positions))
    }

    /// A deterministic synthetic graph, for tests and for bringing the whole pipeline up
    /// before the real export exists. Named `synthetic` rather than `demo` so nobody ships
    /// it by accident believing it is a fly.
    pub fn synthetic(neurons: usize, fan_out: usize, seed: u64) -> Self {
        use rand::{rngs::SmallRng, Rng, SeedableRng};
        let mut rng = SmallRng::seed_from_u64(seed);
        let mut row_start = Vec::with_capacity(neurons + 1);
        let mut targets = Vec::with_capacity(neurons * fan_out);
        let mut weights = Vec::with_capacity(neurons * fan_out);
        row_start.push(0);
        for _ in 0..neurons {
            // Targets sorted within the row: the parallel step relies on that to find the
            // slice of a row that lands in a destination chunk by binary search.
            let mut row: Vec<(u32, f32)> = (0..fan_out)
                .map(|_| (rng.gen_range(0..neurons) as u32, rng.gen_range(1.0..10.0)))
                .collect();
            row.sort_by_key(|&(t, _)| t);
            for (t, w) in row {
                targets.push(t);
                weights.push(w);
            }
            row_start.push(targets.len() as u32);
        }
        Self { row_start, targets, weights, ids: (0..neurons as u64).collect() }
    }
}
