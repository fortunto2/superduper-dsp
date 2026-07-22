//! Phase-vocoder pitch shifter (Track mode) — transposes **polyphonic**
//! material (mixes, chords, drums, whole songs), which the monophonic PSOLA
//! engine can't do.
//!
//! Classic STFT phase-vocoder in the style of Stephan Bernsee's
//! `smbPitchShift`: analyse overlapping windows, estimate each bin's true
//! frequency from the phase advance, move the bins to `bin × α` positions
//! (`α = 2^(Pitch/12)`), re-accumulate the synthesis phase, iFFT and
//! overlap-add. Works on any signal because it re-pitches every spectral
//! component independently.
//!
//! The STFT scaffolding (windowing, rings, FFT plans, OLA, COLA normalisation)
//! is the shared [`synth_core::spectral::StftProcessor`]; this file is just the
//! per-frame pitch-shift operation + the Dry/Wet / latency-padding wrapper.

use crate::dsp::PitchParams;
use realfft::num_complex::Complex;
use superduper_synth_core::dsp_blocks::LatencyDelay;
use superduper_synth_core::spectral::StftProcessor;

/// FFT window. 2048 @ 48 kHz = 23 Hz/bin — enough resolution for bass in a mix.
pub const N: usize = 2048;
/// Hop = N/4 (75 % overlap).
pub const HOP: usize = 512;
/// Intrinsic latency of the STFT OLA (samples). The engine can be padded to a
/// larger reported latency via `PhaseVocoder::new`'s `target_latency`.
pub const LATENCY: usize = N - HOP;

const HALF: usize = N / 2;

/// Per-channel phase-vocoder state.
struct PitchState {
    last_phase: Box<[f32]>, // HALF+1
    sum_phase: Box<[f32]>,  // HALF+1
    prev_magn: Box<[f32]>,  // HALF+1 (spectral-flux transient detection)
}
impl PitchState {
    fn new() -> Self {
        Self {
            last_phase: vec![0.0; HALF + 1].into_boxed_slice(),
            sum_phase: vec![0.0; HALF + 1].into_boxed_slice(),
            prev_magn: vec![0.0; HALF + 1].into_boxed_slice(),
        }
    }
    fn reset(&mut self) {
        self.last_phase.fill(0.0);
        self.sum_phase.fill(0.0);
        self.prev_magn.fill(0.0);
    }
}

/// Shared per-frame scratch (reused across channels — the ops run sequentially).
struct PitchScratch {
    ana_magn: Box<[f32]>,
    ana_freq: Box<[f32]>,
    ana_phase: Box<[f32]>,
    syn_magn: Box<[f32]>,
    syn_freq: Box<[f32]>,
    env: Box<[f32]>,
}
impl PitchScratch {
    fn new() -> Self {
        let z = || vec![0.0f32; HALF + 1].into_boxed_slice();
        Self {
            ana_magn: z(),
            ana_freq: z(),
            ana_phase: z(),
            syn_magn: z(),
            syn_freq: z(),
            env: z(),
        }
    }
}

/// The per-frame pitch-shift operation: read the analysis spectrum, fill the
/// synthesis spectrum. Behaviour identical to the pre-refactor inline version.
#[allow(clippy::too_many_arguments)]
fn pitch_frame(
    ana: &[Complex<f32>],
    syn: &mut [Complex<f32>],
    st: &mut PitchState,
    sc: &mut PitchScratch,
    alpha: f32,
    beta: f32,
    expct: f32,
    freq_per_bin: f32,
    osamp: f32,
) {
    // --- per-bin magnitude + true frequency (raw |X|, no ×2) ---
    for k in 0..=HALF {
        let re = ana[k].re;
        let im = ana[k].im;
        let magn = (re * re + im * im).sqrt();
        let phase = im.atan2(re);
        sc.ana_phase[k] = phase;
        let mut tmp = phase - st.last_phase[k];
        st.last_phase[k] = phase;
        tmp -= k as f32 * expct;
        let mut qpd = (tmp / core::f32::consts::PI) as i32;
        if qpd >= 0 {
            qpd += qpd & 1;
        } else {
            qpd -= qpd & 1;
        }
        tmp -= core::f32::consts::PI * qpd as f32;
        tmp = osamp * tmp / core::f32::consts::TAU;
        sc.ana_magn[k] = magn;
        sc.ana_freq[k] = k as f32 * freq_per_bin + tmp * freq_per_bin;
    }

    // --- transient detection (spectral flux) ---
    let mut flux = 0.0f32;
    let mut total = 0.0f32;
    for k in 0..=HALF {
        let d = sc.ana_magn[k] - st.prev_magn[k];
        if d > 0.0 {
            flux += d;
        }
        total += sc.ana_magn[k];
        st.prev_magn[k] = sc.ana_magn[k];
    }
    let onset = total > 1e-4 && flux > 0.40 * total;

    // --- pitch shift: move bin k → round(k·α) ---
    for v in sc.syn_magn.iter_mut() {
        *v = 0.0;
    }
    for v in sc.syn_freq.iter_mut() {
        *v = 0.0;
    }
    for k in 0..=HALF {
        let index = (k as f32 * alpha).round() as usize;
        if index <= HALF {
            sc.syn_magn[index] += sc.ana_magn[k];
            sc.syn_freq[index] = sc.ana_freq[k] * alpha;
        }
    }

    // --- optional formant (envelope) shift by β ---
    if (beta - 1.0).abs() > 1e-3 {
        const W: usize = 8;
        for k in 0..=HALF {
            let lo = k.saturating_sub(W);
            let hi = (k + W).min(HALF);
            let mut s = 0.0;
            for j in lo..=hi {
                s += sc.syn_magn[j];
            }
            sc.env[k] = s / (hi - lo + 1) as f32 + 1e-9;
        }
        for k in 0..=HALF {
            let src = (k as f32 / beta).round() as usize;
            let shifted_env = if src <= HALF { sc.env[src] } else { 0.0 };
            sc.syn_magn[k] *= shifted_env / sc.env[k];
        }
    }

    // --- synthesis: accumulate phase (or reset on a transient) ---
    for k in 0..=HALF {
        let magn = sc.syn_magn[k];
        if onset {
            st.sum_phase[k] = sc.ana_phase[k];
        } else {
            let mut tmp = sc.syn_freq[k];
            tmp -= k as f32 * freq_per_bin;
            tmp /= freq_per_bin;
            tmp = core::f32::consts::TAU * tmp / osamp;
            tmp += k as f32 * expct;
            st.sum_phase[k] += tmp;
        }
        let phase = st.sum_phase[k];
        syn[k] = Complex::new(magn * phase.cos(), magn * phase.sin());
    }
}

pub struct PhaseVocoder {
    stft: [StftProcessor; 2],
    st: [PitchState; 2],
    sc: PitchScratch,
    expct: f32,
    freq_per_bin: f32,
    osamp: f32,
    /// Dry delay (per channel), sized to `target_latency`.
    dry: [LatencyDelay; 2],
    /// Extra wet delay (per channel) padding LATENCY up to `target_latency`.
    wet: [LatencyDelay; 2],
    wet_extra: usize,
    target_latency: usize,
}

impl PhaseVocoder {
    pub fn new(sr: f32, target_latency: usize) -> Self {
        let target_latency = target_latency.max(LATENCY);
        let wet_extra = target_latency - LATENCY;
        let stft0 = StftProcessor::new(sr, N, HOP, 1);
        let expct = stft0.expct();
        let freq_per_bin = stft0.freq_per_bin();
        let osamp = stft0.osamp();
        Self {
            stft: [stft0, StftProcessor::new(sr, N, HOP, 1)],
            st: [PitchState::new(), PitchState::new()],
            sc: PitchScratch::new(),
            expct,
            freq_per_bin,
            osamp,
            dry: [LatencyDelay::new(target_latency), LatencyDelay::new(target_latency)],
            wet: [LatencyDelay::new(wet_extra), LatencyDelay::new(wet_extra)],
            wet_extra,
            target_latency,
        }
    }

    pub fn reset(&mut self) {
        for s in self.stft.iter_mut() {
            s.reset();
        }
        for s in self.st.iter_mut() {
            s.reset();
        }
        for d in self.dry.iter_mut() {
            d.reset();
        }
        for w in self.wet.iter_mut() {
            w.reset();
        }
    }

    pub fn latency(&self) -> usize {
        self.target_latency
    }

    /// Process one stereo block (Dry/Wet, output trim, bypass handled here).
    pub fn process(
        &mut self,
        in_l: &[f32],
        in_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
        p: &PitchParams,
    ) {
        let n = in_l.len().min(out_l.len());
        if p.bypassed {
            out_l[..n].copy_from_slice(&in_l[..n]);
            let rn = n.min(in_r.len()).min(out_r.len());
            out_r[..rn].copy_from_slice(&in_r[..rn]);
            return;
        }
        let alpha = 2f32.powf(p.pitch_st / 12.0);
        let beta = 2f32.powf(p.formant_st / 12.0);

        let PhaseVocoder {
            stft,
            st,
            sc,
            expct,
            freq_per_bin,
            osamp,
            dry,
            wet,
            wet_extra,
            ..
        } = self;
        let (expct, freq_per_bin, osamp) = (*expct, *freq_per_bin, *osamp);

        for i in 0..n {
            let xl = in_l[i];
            let xr = *in_r.get(i).unwrap_or(&xl);

            let mut wet_l = stft[0].process_sample(&[xl], |ana, syn| {
                pitch_frame(ana[0], syn, &mut st[0], sc, alpha, beta, expct, freq_per_bin, osamp)
            });
            let mut wet_r = stft[1].process_sample(&[xr], |ana, syn| {
                pitch_frame(ana[0], syn, &mut st[1], sc, alpha, beta, expct, freq_per_bin, osamp)
            });

            // Pad the intrinsic-LATENCY wet up to target_latency.
            if *wet_extra > 0 {
                wet_l = wet[0].process(wet_l);
                wet_r = wet[1].process(wet_r);
            }

            // Latency-matched dry (read oldest, then overwrite with newest).
            let dl = dry[0].process(xl);
            let dr = dry[1].process(xr);

            if !wet_l.is_finite() {
                wet_l = 0.0;
            }
            if !wet_r.is_finite() {
                wet_r = 0.0;
            }
            out_l[i] = (dl * (1.0 - p.mix) + wet_l * p.mix) * p.output_lin;
            if i < out_r.len() {
                out_r[i] = (dr * (1.0 - p.mix) + wet_r * p.mix) * p.output_lin;
            }
        }
    }
}
