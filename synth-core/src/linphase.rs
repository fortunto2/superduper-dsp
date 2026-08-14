//! Linear-phase FIR design + overlap-add convolution.
//!
//! Used by the LinPhase EQ for mastering — where phase coherence
//! across the spectrum matters more than the millisecond of latency
//! a long FIR introduces.
//!
//! Design pipeline:
//! 1. Caller samples desired magnitude response at `N/2 + 1` bins
//!    (where N is the FIR length). Magnitudes are LINEAR, not dB.
//! 2. `design_linear_phase_fir` builds the complex spectrum (real
//!    magnitude × zero phase), inverse-FFTs to a real impulse, then
//!    applies a Hann window to suppress sidelobes.
//! 3. The result is a symmetric (linear-phase) FIR of length N. Group
//!    delay = `N/2` samples (constant across the spectrum — that's
//!    the whole point).
//!
//! Process pipeline:
//! - `OverlapAddConvolver` holds the FIR + a circular history buffer.
//!   Per sample: shift in input, dot-product with FIR, return output.
//! - For long FIRs (4096+) this should be FFT-based overlap-add; we
//!   ship the direct form because (a) mastering FIRs are typically
//!   1024-2048 taps and (b) it's RT-safe with no allocation.

use realfft::num_complex::Complex;
use realfft::RealFftPlanner;

/// Build a symmetric linear-phase FIR of length `fir_len` from a
/// target magnitude response sampled at `fir_len/2 + 1` evenly-spaced
/// frequency bins (0 Hz, sr/fir_len, 2·sr/fir_len, …, Nyquist).
///
/// The magnitudes are LINEAR (not dB) — convert before calling.
/// Hann window applied at the end to keep sidelobes at -32 dB.
///
/// Group delay of the result = `fir_len / 2` samples (constant across
/// all frequencies — that's what "linear phase" means).
pub fn design_linear_phase_fir(target_mag: &[f32], fir_len: usize) -> Vec<f32> {
    let n_bins = fir_len / 2 + 1;
    assert_eq!(
        target_mag.len(),
        n_bins,
        "target_mag must have fir_len/2 + 1 entries"
    );
    // Build complex spectrum: magnitude × zero phase (real positive).
    let mut planner = RealFftPlanner::<f32>::new();
    let ifft = planner.plan_fft_inverse(fir_len);
    let mut spectrum: Vec<Complex<f32>> =
        target_mag.iter().map(|&m| Complex::new(m, 0.0)).collect();
    let mut impulse = vec![0.0f32; fir_len];
    ifft.process(&mut spectrum, &mut impulse)
        .expect("ifft input shape always correct");
    // Normalize the inverse FFT (realfft doesn't scale by 1/N).
    let inv_n = 1.0 / fir_len as f32;
    for x in impulse.iter_mut() {
        *x *= inv_n;
    }
    // The zero-phase impulse comes out centred on sample 0 with the
    // tail wrapping around to the end of the buffer. Shift by N/2
    // to centre it (now it's symmetric around sample N/2 — true
    // linear phase with group delay N/2).
    let half = fir_len / 2;
    impulse.rotate_left(half);
    // Hann window to taper the edges — without this, truncating the
    // theoretically-infinite impulse at fir_len gives Gibbs ringing
    // ~13 dB into the passband.
    let denom = (fir_len - 1).max(1) as f32;
    for (i, x) in impulse.iter_mut().enumerate() {
        let phase = std::f32::consts::PI * 2.0 * i as f32 / denom;
        let w = 0.5 - 0.5 * phase.cos();
        *x *= w;
    }
    impulse
}

/// Direct-form FIR convolver with a circular history buffer. RT-safe
/// — no allocation per sample, just a multiply-accumulate over the
/// stored coefficients.
///
/// For FIRs above ~4k taps, an FFT-based overlap-add convolver is
/// asymptotically cheaper (~log N per sample vs N). For 1k-2k taps
/// (typical mastering EQ range) direct is fine — modern CPUs eat
/// 2000 multiplies per sample at 48 kHz comfortably (≤ 5% of one
/// core at 96 kHz × 2 channels).
pub struct DirectFirConvolver {
    fir: Vec<f32>,
    history: Vec<f32>,
    write_pos: usize,
}

impl DirectFirConvolver {
    pub fn new(fir: Vec<f32>) -> Self {
        let n = fir.len();
        Self {
            fir,
            history: vec![0.0; n],
            write_pos: 0,
        }
    }

    /// Swap in new coefficients. Called from the audio thread, so a length
    /// mismatch keeps the old FIR instead of panicking — a panic here would
    /// take down the host's audio callback, and the rule against panicking in
    /// `process()` does not have an exception for "this should never happen".
    pub fn replace_fir(&mut self, fir: Vec<f32>) -> bool {
        if fir.len() != self.fir.len() {
            return false;
        }
        self.fir = fir;
        // Don't clear history — would click. Coefficient swap is
        // generally smooth as long as the response shape is similar.
        true
    }

    pub fn clear(&mut self) {
        for s in self.history.iter_mut() {
            *s = 0.0;
        }
        self.write_pos = 0;
    }

    pub fn fir_len(&self) -> usize {
        self.fir.len()
    }

    /// Latency in samples for a linear-phase FIR (group delay = N/2).
    pub fn latency_samples(&self) -> u32 {
        (self.fir.len() / 2) as u32
    }

    /// Process one sample. Convolves the new input with the FIR
    /// against the stored history.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let n = self.fir.len();
        self.history[self.write_pos] = x;
        // Walk the FIR backwards from write_pos so coefficient 0 lines
        // up with the newest sample (standard convolution-sum form).
        let mut acc = 0.0f32;
        let mut idx = self.write_pos;
        for &h in self.fir.iter() {
            acc += h * self.history[idx];
            idx = if idx == 0 { n - 1 } else { idx - 1 };
        }
        self.write_pos = (self.write_pos + 1) % n;
        acc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fir_design_round_trip_flat_passes_through() {
        // Target = flat unity magnitude. FIR should be a centred
        // impulse (after windowing — Hann smooths slightly).
        let fir_len = 256;
        let target: Vec<f32> = vec![1.0; fir_len / 2 + 1];
        let fir = design_linear_phase_fir(&target, fir_len);
        assert_eq!(fir.len(), fir_len);
        // The peak should be at the centre (group delay = N/2).
        let peak_idx = fir
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert_eq!(peak_idx, fir_len / 2, "peak should be at FIR centre");
    }

    #[test]
    fn fir_is_symmetric() {
        let fir_len = 256;
        let target: Vec<f32> = (0..fir_len / 2 + 1)
            .map(|i| 1.0 + 0.5 * (i as f32 / (fir_len / 2) as f32))
            .collect();
        let fir = design_linear_phase_fir(&target, fir_len);
        // For an even-length linear-phase FIR with the peak at
        // index N/2 (our rotation strategy), the symmetry axis is
        // index N/2 itself: fir[N/2 + k] = fir[N/2 - k] for k>=1.
        // Index 0 has no mirror (it's the lone "tail" sample).
        let centre = fir_len / 2;
        for k in 1..centre {
            let a = fir[centre + k];
            let b = fir[centre - k];
            assert!(
                (a - b).abs() < 1e-4,
                "asymmetry at k={k}: fir[{}]={a}, fir[{}]={b}",
                centre + k,
                centre - k
            );
        }
    }

    #[test]
    fn convolver_impulse_response_matches_fir() {
        // Feed an impulse, output should equal the FIR samples in order.
        let fir = vec![0.1, 0.3, 0.5, 0.3, 0.1];
        let mut conv = DirectFirConvolver::new(fir.clone());
        let mut out = Vec::new();
        out.push(conv.process(1.0));
        for _ in 1..fir.len() {
            out.push(conv.process(0.0));
        }
        for (i, (a, b)) in fir.iter().zip(out.iter()).enumerate() {
            assert!((a - b).abs() < 1e-6, "impulse mismatch at {i}: {a} vs {b}");
        }
    }

    #[test]
    fn convolver_silence_in_silence_out() {
        let fir = vec![0.1, 0.3, 0.5, 0.3, 0.1];
        let mut conv = DirectFirConvolver::new(fir);
        for _ in 0..100 {
            assert_eq!(conv.process(0.0), 0.0);
        }
    }
}
