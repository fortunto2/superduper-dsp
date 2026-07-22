//! WSOLA time-stretch (Waveform Similarity Overlap-Add).
//!
//! Pitch-preserving tempo change for beat stems. Synthesis advances by a
//! fixed hop `Hs`; the analysis read position is anchored to `output/ratio`
//! but nudged within a small search window to the offset whose waveform best
//! continues the previously written frame — that similarity search is what
//! keeps transients crisp and avoids the phase smearing of naive OLA.
//!
//! Stereo is stretched with a *single* lag decision taken on the L+R sum so
//! the image stays coherent. Output length is exactly `round(input * ratio)`.
//!
//! Chosen over a C++ binding (signalsmith-stretch / ssstretch pull in
//! bindgen + libclang + a C++ toolchain) to keep the tool hermetic and pure
//! Rust. Good enough for drum/bass/other stems, which is all v0 stretches.

/// Half-wave-rectified normalised cross-correlation of two equal-length
/// mono slices. Returns a similarity in roughly [-1, 1]; higher = better
/// match. Zero-energy slices score 0.
#[inline]
fn xcorr(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut ea = 0.0f32;
    let mut eb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        ea += x * x;
        eb += y * y;
    }
    let denom = (ea * eb).sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        dot / denom
    }
}

/// Read `len` mono (L+R) samples from `l`/`r` starting at `start` (may be
/// negative or past the end — out-of-range reads as 0). Fills `buf`.
#[inline]
fn read_mono(l: &[f32], r: &[f32], start: i64, buf: &mut [f32]) {
    let n = l.len() as i64;
    for (k, slot) in buf.iter_mut().enumerate() {
        let idx = start + k as i64;
        *slot = if idx >= 0 && idx < n {
            l[idx as usize] + r[idx as usize]
        } else {
            0.0
        };
    }
}

/// Time-stretch a stereo buffer by `ratio` (output length ≈ input × ratio).
/// `ratio == 1.0` (within 1e-6) is a fast no-op clone.
pub fn time_stretch_stereo(l: &[f32], r: &[f32], ratio: f64, sr: u32) -> (Vec<f32>, Vec<f32>) {
    let n = l.len().min(r.len());
    if (ratio - 1.0).abs() < 1e-6 || n == 0 {
        return (l[..n].to_vec(), r[..n].to_vec());
    }
    let out_len = ((n as f64) * ratio).round() as usize;
    if out_len == 0 {
        return (Vec::new(), Vec::new());
    }

    // Frame ~46 ms, 50 % overlap → Hann sums to unity on overlap-add.
    let w = (((sr as f64) * 0.046).round() as usize).max(256) & !1;
    let hs = w / 2; // synthesis hop
    let ov = w - hs; // overlap length
    // Correlation window + search radius, capped so the O(frames·search·cw)
    // cost stays a couple of seconds even on a full track.
    let cw = ov.min(512);
    let search = ((sr as f64) * 0.012).round() as i64; // ±12 ms

    // Hann window.
    let hann: Vec<f32> = (0..w)
        .map(|i| {
            let x = std::f32::consts::PI * i as f32 / (w as f32 - 1.0);
            x.sin() * x.sin()
        })
        .collect();

    let mut out_l = vec![0.0f32; out_len + w];
    let mut out_r = vec![0.0f32; out_len + w];

    // Overlap-add one windowed input frame starting at input index `ia`.
    let ola = |out_l: &mut [f32], out_r: &mut [f32], op: usize, ia: i64| {
        for k in 0..w {
            let idx = ia + k as i64;
            if idx >= 0 && (idx as usize) < n {
                let g = hann[k];
                out_l[op + k] += l[idx as usize] * g;
                out_r[op + k] += r[idx as usize] * g;
            }
        }
    };

    // First frame at input 0.
    let mut ia: i64 = 0;
    ola(&mut out_l, &mut out_r, 0, ia);

    let mut target = vec![0.0f32; cw];
    let mut cand = vec![0.0f32; cw];

    let mut m = 1usize;
    loop {
        let op = m * hs;
        if op >= out_len {
            break;
        }
        // Natural continuation of the previously placed frame — the samples
        // that fall in the coming overlap region.
        read_mono(l, r, ia + hs as i64, &mut target);

        // Nominal analysis position anchors the average stretch to `ratio`.
        let nominal = ((op as f64) / ratio).round() as i64;

        // Search for the offset whose head best matches the continuation.
        let mut best_delta = 0i64;
        let mut best_score = f32::NEG_INFINITY;
        let mut delta = -search;
        while delta <= search {
            read_mono(l, r, nominal + delta, &mut cand);
            let score = xcorr(&cand, &target);
            if score > best_score {
                best_score = score;
                best_delta = delta;
            }
            delta += 1;
        }

        ia = (nominal + best_delta).clamp(0, (n as i64 - 1).max(0));
        ola(&mut out_l, &mut out_r, op, ia);
        m += 1;
    }

    out_l.truncate(out_len);
    out_r.truncate(out_len);
    (out_l, out_r)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44_100;

    /// Index of the largest-magnitude sample in a window.
    fn peak_index(x: &[f32]) -> usize {
        let mut bi = 0;
        let mut bv = 0.0f32;
        for (i, v) in x.iter().enumerate() {
            if v.abs() > bv {
                bv = v.abs();
                bi = i;
            }
        }
        bi
    }

    #[test]
    fn output_length_is_exactly_ratio_scaled() {
        let n = SR as usize; // 1 s
        let l: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 200.0 * i as f32 / SR as f32).sin())
            .collect();
        let (out_l, out_r) = time_stretch_stereo(&l, &l, 1.05, SR);
        let expect = ((n as f64) * 1.05).round() as usize;
        assert_eq!(out_l.len(), expect);
        assert_eq!(out_r.len(), expect);
    }

    #[test]
    fn unity_ratio_is_identity() {
        let l: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin()).collect();
        let (out_l, _) = time_stretch_stereo(&l, &l, 1.0, SR);
        assert_eq!(out_l.len(), l.len());
        for i in 0..l.len() {
            assert!((out_l[i] - l[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn click_spacing_scales_by_ratio() {
        // Two clicks spaced 0.5 s apart; stretched by 1.05 the spacing must
        // grow by ~1.05. WSOLA locks onto the transients, so the measured
        // ratio lands within a couple percent of exact.
        let ratio = 1.05;
        let n = SR as usize; // 1 s
        let p0 = SR as usize / 4; // 0.25 s
        let p1 = p0 + SR as usize / 2; // 0.75 s (spacing 0.5 s)
        let mut x = vec![0.0f32; n];
        // Short decaying clicks so they survive windowing as clear peaks.
        for (p, _) in [(p0, ()), (p1, ())] {
            for k in 0..64 {
                let env = 1.0 - k as f32 / 64.0;
                x[p + k] = env * (2.0 * std::f32::consts::PI * 1500.0 * k as f32 / SR as f32).sin();
            }
        }
        let (out, _) = time_stretch_stereo(&x, &x, ratio, SR);

        // Locate the two clicks in the output by peak-picking each half.
        let mid = out.len() / 2;
        let q0 = peak_index(&out[..mid]);
        let q1 = mid + peak_index(&out[mid..]);

        let in_spacing = (p1 - p0) as f64;
        let out_spacing = (q1 - q0) as f64;
        let measured = out_spacing / in_spacing;
        assert!(
            (measured - ratio).abs() < 0.03,
            "click spacing ratio {measured} should be ~{ratio} (q0={q0}, q1={q1})"
        );
    }
}
