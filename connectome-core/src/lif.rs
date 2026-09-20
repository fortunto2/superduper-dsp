//! Leaky integrate-and-fire: the whole model is four lines of arithmetic per neuron.
//!
//! Voltage decays toward rest, incoming spikes add to it, crossing the threshold emits a
//! spike and resets, and a refractory counter holds the cell quiet for a moment after.
//! Shiu et al. (Nature, Oct 2024) showed this much predicts real fly responses well; the
//! biology it omits is omitted on purpose.

use crate::graph::Graph;
use rayon::prelude::*;
use std::sync::Arc;

/// Below this many cells the thread hand-off costs more than it saves (the eco tier is 700).
const PARALLEL_MIN_NEURONS: usize = 20_000;
/// Cells per parallel work item.
const CHUNK: usize = 8_192;
/// Per-step decay of the running firing rate: a time constant of ~100 steps (100 ms at
/// dt = 1), long enough to read as a heat map, short enough to follow a touch.
const RATE_DECAY: f32 = 0.99;

#[derive(Debug, Clone, Copy)]
pub struct LifParams {
    /// Membrane time constant, ms.
    pub tau_m: f32,
    pub v_rest: f32,
    pub v_threshold: f32,
    pub v_reset: f32,
    /// Refractory period, ms.
    pub t_refractory: f32,
    /// Millivolts per synapse, scaling `syn_count` into a voltage step.
    pub weight_scale: f32,
    /// Background drive, so a silent graph is not mistaken for a broken one.
    pub noise: f32,
    pub dt: f32,
}

impl Default for LifParams {
    fn default() -> Self {
        Self {
            tau_m: 20.0,
            v_rest: -52.0,
            v_threshold: -45.0,
            v_reset: -52.0,
            t_refractory: 2.2,
            weight_scale: 0.275,
            noise: 0.0,
            dt: 1.0,
        }
    }
}

pub struct Lif {
    /// Shared: every fly in a colony has the same wiring and differs only in its state, so
    /// the 31 MB graph is carried once however many brains are running over it.
    graph: Arc<Graph>,
    params: LifParams,
    v: Vec<f32>,
    refractory: Vec<f32>,
    /// Current injected from outside — sensory input, or a plugin's odour vector.
    pub stimulus: Vec<f32>,
    spiked: Vec<u32>,
    accumulator: Vec<f32>,
    /// Running firing rate per cell, 0..1 as "fraction of recent steps it fired".
    rate: Vec<f32>,
    /// Per-thread scatter targets for the parallel step, kept between steps: allocating
    /// 550 KB per thread per millisecond is not free.
    seed: u64,
    steps: u64,
    total_spikes: u64,
}

/// Counter-based uniform noise in [-1, 1): a hash of (seed, step, cell), so every cell's
/// noise is a pure function of where and when — identical whether one thread or eight
/// compute it, which is what keeps a run reproducible from its seed.
#[inline]
fn unit_noise(seed: u64, step: u64, cell: usize) -> f32 {
    let mut z = seed ^ step.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (cell as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    ((z >> 40) as f32) * (2.0 / 16_777_216.0) - 1.0
}

/// One cell's update for one step. Shared by the serial and parallel paths so they cannot
/// drift apart. Returns whether the cell fired.
#[inline]
fn update_cell(p: &LifParams, decay: f32, seed: u64, step: u64, i: usize,
               v: &mut f32, refractory: &mut f32, input: f32) -> bool {
    if *refractory > 0.0 {
        *refractory -= p.dt;
        *v = p.v_reset;
        return false;
    }
    let mut nv = p.v_rest + (*v - p.v_rest) * decay + input;
    if p.noise > 0.0 {
        nv += unit_noise(seed, step, i) * p.noise;
    }
    if nv >= p.v_threshold {
        *v = p.v_reset;
        *refractory = p.t_refractory;
        true
    } else {
        *v = nv;
        false
    }
}

impl Lif {
    pub fn new(graph: impl Into<Arc<Graph>>, params: LifParams, seed: u64) -> Self {
        let graph = graph.into();
        let n = graph.neurons();
        Self {
            graph,
            params,
            v: vec![params.v_rest; n],
            refractory: vec![0.0; n],
            stimulus: vec![0.0; n],
            spiked: Vec::with_capacity(n / 8),
            accumulator: vec![0.0; n],
            rate: vec![0.0; n],
            seed,
            steps: 0,
            total_spikes: 0,
        }
    }

    pub fn graph(&self) -> &Graph {
        &self.graph
    }

    pub fn params(&self) -> &LifParams {
        &self.params
    }

    /// The wiring, for another brain to share rather than copy.
    pub fn shared_graph(&self) -> Arc<Graph> {
        Arc::clone(&self.graph)
    }

    pub fn voltages(&self) -> &[f32] {
        &self.v
    }

    /// Indices that fired on the most recent step.
    pub fn spikes(&self) -> &[u32] {
        &self.spiked
    }

    /// Running firing rate per cell (see `RATE_DECAY`): the heat map's data.
    pub fn rates(&self) -> &[f32] {
        &self.rate
    }

    pub fn steps(&self) -> u64 {
        self.steps
    }

    pub fn total_spikes(&self) -> u64 {
        self.total_spikes
    }

    /// Advance one `dt`. Returns how many neurons fired.
    pub fn step(&mut self) -> usize {
        let p = self.params;
        let decay = (-p.dt / p.tau_m).exp();
        let n = self.v.len();
        let parallel = n >= PARALLEL_MIN_NEURONS;

        // Deliver last step's spikes along their out-edges. Walking only the cells that
        // fired is what keeps this cheap: a fly brain is quiet most of the time, so this
        // touches a few percent of the edges per step rather than all of them.
        if parallel && self.spiked.len() >= 256 {
            // Parallel over DESTINATION ranges, not over sources. The earlier
            // source-split version summed each target's contributions grouped
            // by thread-chunk, so f32 addition order — and therefore the spike
            // train of a chaotic system — depended on rayon's thread count.
            // Here every worker walks the same spiked list in the same order
            // and keeps only the targets in its own accumulator slice: the
            // sum for any given cell is always taken in spiked order, whatever
            // the machine. That is what makes a receipt from a phone checkable
            // against a Mac. Costs: each worker scans every spiking row (reads
            // scale with thread count), but only spiking rows — a few percent
            // of edges on a quiet brain — and the per-thread scratch vectors
            // (threads × n floats, ~4.5 MB on the full tier) are gone.
            let graph = &self.graph;
            let spiked = &self.spiked;
            let scale = p.weight_scale;
            self.accumulator.par_chunks_mut(CHUNK).enumerate().for_each(|(c, out)| {
                let base = c * CHUNK;
                let end = base + out.len();
                out.iter_mut().for_each(|a| *a = 0.0);
                for &src in spiked {
                    let lo = graph.row_start[src as usize] as usize;
                    let hi = graph.row_start[src as usize + 1] as usize;
                    for i in lo..hi {
                        let t = graph.targets[i] as usize;
                        if t >= base && t < end {
                            out[t - base] += graph.weights[i] * scale;
                        }
                    }
                }
            });
        } else {
            self.accumulator.iter_mut().for_each(|a| *a = 0.0);
            for &src in &self.spiked {
                let lo = self.graph.row_start[src as usize] as usize;
                let hi = self.graph.row_start[src as usize + 1] as usize;
                for i in lo..hi {
                    let dst = self.graph.targets[i] as usize;
                    self.accumulator[dst] += self.graph.weights[i] * p.weight_scale;
                }
            }
        }

        let (seed, step) = (self.seed, self.steps);
        self.spiked.clear();
        if parallel {
            let fired: Vec<Vec<u32>> = self
                .v
                .par_chunks_mut(CHUNK)
                .zip(self.refractory.par_chunks_mut(CHUNK))
                .zip(self.rate.par_chunks_mut(CHUNK))
                .zip(self.accumulator.par_chunks(CHUNK))
                .zip(self.stimulus.par_chunks(CHUNK))
                .enumerate()
                .map(|(c, ((((v, r), rate), acc), stim))| {
                    let base = c * CHUNK;
                    let mut out = Vec::new();
                    for k in 0..v.len() {
                        let i = base + k;
                        let f = update_cell(&p, decay, seed, step, i, &mut v[k], &mut r[k], acc[k] + stim[k]);
                        rate[k] = rate[k] * RATE_DECAY + if f { 1.0 - RATE_DECAY } else { 0.0 };
                        if f {
                            out.push(i as u32);
                        }
                    }
                    out
                })
                .collect();
            for f in fired {
                self.spiked.extend(f);
            }
        } else {
            for i in 0..n {
                let input = self.accumulator[i] + self.stimulus[i];
                let f = update_cell(&p, decay, seed, step, i, &mut self.v[i], &mut self.refractory[i], input);
                self.rate[i] = self.rate[i] * RATE_DECAY + if f { 1.0 - RATE_DECAY } else { 0.0 };
                if f {
                    self.spiked.push(i as u32);
                }
            }
        }

        self.steps += 1;
        self.total_spikes += self.spiked.len() as u64;
        self.spiked.len()
    }

    /// Run `n` steps, returning the spike count per step. The per-step vector is the
    /// comparable artefact: a mutation test needs the shape of the train, not a total,
    /// because two very different trains can sum to the same number.
    pub fn run(&mut self, n: usize) -> Vec<u32> {
        (0..n).map(|_| self.step() as u32).collect()
    }

    pub fn set_stimulus(&mut self, neuron: usize, current: f32) {
        if let Some(slot) = self.stimulus.get_mut(neuron) {
            *slot = current;
        }
    }

    /// Clears sensory input only. Background drive lives in `noise`, so a brain kept alive
    /// by its own spontaneous activity is not silenced when a touch ends.
    pub fn clear_stimulus(&mut self) {
        self.stimulus.iter_mut().for_each(|s| *s = 0.0);
    }

    /// Background drive, the arousal dial. Measured on the synthetic graph at
    /// `weight_scale` 0.15 (`examples/noise_sweep.rs`): 1.0 is silent, 2.0 fires 1% of cells
    /// a step, 3.5 about 5%, 8.0 about 8%. A silent brain restarts on its own once raised.
    pub fn set_noise(&mut self, noise: f32) {
        self.params.noise = noise.max(0.0);
    }

    pub fn noise(&self) -> f32 {
        self.params.noise
    }

    /// Zero a contiguous bundle of synapses. Exists for the mutation test rather than for
    /// biology: if destroying weights does not change the spike train, the simulation is
    /// reporting its own defaults and not this connectome.
    /// Lesioning gives this brain its own copy of the wiring, so a colony sharing one graph
    /// cannot have a mutation test quietly damage every fly at once.
    pub fn lesion_synapses(&mut self, from: usize, count: usize) {
        let graph = Arc::make_mut(&mut self.graph);
        let end = (from + count).min(graph.weights.len());
        for w in &mut graph.weights[from..end] {
            *w = 0.0;
        }
    }

    pub fn reset(&mut self, seed: u64) {
        // Stimulus is cleared too: a touch injected before reset must not keep
        // driving cells after it, or the run is no longer reproducible from
        // its seed — which is the whole reason reset takes one.
        self.stimulus.iter_mut().for_each(|s| *s = 0.0);
        self.v.iter_mut().for_each(|v| *v = self.params.v_rest);
        self.refractory.iter_mut().for_each(|r| *r = 0.0);
        self.rate.iter_mut().for_each(|r| *r = 0.0);
        self.spiked.clear();
        self.seed = seed;
        self.steps = 0;
        self.total_spikes = 0;
    }
}
