//! SuperDuper Vocoder — classic multi-band channel vocoder DSP.
//!
//! Robot-voice character in the Daft Punk / Kraftwerk lineage (the real
//! records used a Sennheiser VSM201 / EMS; this models the same ideas). The
//! DSP here is pure and CLAP-free (the plumbing lives in `lib.rs`) so it can
//! be driven directly from `tests/`.
//!
//! ## Signal flow
//!
//! 1. **Modulator** = the main input, summed L+R → mono (the voice being
//!    vocoded). Mono-summing is the simplest correct choice — a vocoder cares
//!    about the modulator's *spectral envelope*, which is a mono quantity.
//! 2. **Analysis bank** — `active_bands` constant-Q band-pass filters (two
//!    cascaded RBJ biquads each = 4th order, unity peak gain) split the
//!    modulator; each band drives an asymmetric attack/release envelope
//!    follower. Band centres are mel-spaced from `F_LO`..`F_HI`, denser in
//!    the 300 Hz–3 kHz formant region than plain log spacing.
//! 3. **Carrier** — either internal oscillators (saw / square / pulse /
//!    saw+sub, pitch-tracked off the modulator by YIN so you can vocode
//!    without a keyboard) or the sidechain input (port 1, mono-summed). The
//!    internal carrier is **stereo**: two detuned voices panned L/R for width
//!    (`Carrier Detune`); the saw+sub octave stays centred.
//! 4. **Synthesis banks** — the same band centres (optionally formant-shifted)
//!    filter the carrier, one bank per channel; each band is multiplied by
//!    that band's modulator envelope and summed → the vocoded signal.
//! 5. **Unvoiced (noise) excitation** — for the upper (sibilant) bands the
//!    carrier is cross-faded toward **band-filtered white noise** by
//!    `Unvoiced Mix` (the VSM201 trick used on "Around the World"). Gated by
//!    each band's envelope, so noise only appears where the voice actually has
//!    high-frequency energy — natural s / t / f consonants, far cleaner than
//!    high-passing the dry voice. Two independent noise streams give the
//!    sibilants stereo width.
//! 6. **Drive** — `tanh_drive` on each summed vocoded channel (symmetric, so
//!    no DC blocker needed) for robot grit.
//! 7. **Output** — Dry/Wet blend + output trim.
//!
//! The wet (vocoded) path is stereo; the dry path stays stereo too.

use realfft::num_complex::Complex;
use superduper_synth_core::dsp_blocks::{
    median_small, midi_note_to_hz, tanh_drive, Biquad, EnvelopeDetector, LatencyDelay, SmoothedParam,
};
use superduper_synth_core::pitch::YinPitchTracker;
use superduper_synth_core::spectral::StftProcessor;

/// Vocoder `Mode` enum values. Classic = the multi-band channel vocoder;
/// Spectral = FFT cross-synthesis (finer, whole-spectrum envelope transfer).
pub const MODE_CLASSIC: u32 = 0;
pub const MODE_SPECTRAL: u32 = 1;

/// STFT geometry for Spectral mode (shared `StftProcessor`).
const STFT_N: usize = 2048;
const STFT_HOP: usize = 512;
/// Reported latency of Spectral mode (samples). Classic is 0-latency, but the
/// plugin reports this for both so switching Mode never re-triggers host PDC.
pub const STFT_LATENCY: usize = STFT_N - STFT_HOP;

/// Ceiling on the cross-synthesis gain — stops the mod/carrier envelope ratio
/// from exploding in carrier spectral nulls (which would amplify the noise
/// floor into harshness — and blow the output level up on sparse carriers).
const SPEC_GAIN_CEIL: f32 = 4.0;

/// Weight of the unvoiced breath injected into the Spectral top. Kept low (and
/// the noise itself is frequency-darkened below) so consonants read without a
/// harsh hiss layer — the >4-5 kHz sharpness the user flagged.
const SPEC_NOISE_W: f32 = 0.2;

/// HF attack-smoothing band + coefficients (per STFT hop ≈ 10.7 ms). Above
/// `TSMOOTH_LO_HZ` the modulator envelope is blended toward a slow-attack /
/// fast-release temporal average (full weight by `TSMOOTH_HI_HZ`), so a sudden
/// onset ramps the top in gently instead of throwing a hard, sharp HF edge.
const TSMOOTH_LO_HZ: f32 = 2500.0;
const TSMOOTH_HI_HZ: f32 = 6000.0;
const TSMOOTH_ATTACK: f32 = 0.72; // slow rise (~32 ms) — soft attack
const TSMOOTH_RELEASE: f32 = 0.25; // faster fall

/// HF roll-off tilt on the Spectral wet so the top matches the natural fade of
/// the Classic mel band bank (top band ≈ 8 kHz, then near-nothing). The raw
/// envelope-transfer passes the whole carrier spectrum (a saw reaches Nyquist),
/// so its top sat ~+16 dB above Classic → the "sharp above 4-5 kHz" the user
/// heard (bright carrier HF harmonics, not noise). A power-law tilt above the
/// corner (~−14 dB/oct: unity ≤4 kHz, ≈−14 dB at 8 kHz, ≈−18 at 10 kHz, floored
/// at −24 dB) brings the top within a few dB of Classic while keeping a little
/// air for consonants.
#[inline]
fn hf_shelf(fc: f32) -> f32 {
    const CORNER: f32 = 3000.0;
    const EXP: f32 = 3.0; // ≈ −18 dB/octave above the corner
    const FLOOR: f32 = 0.04; // ≈ −28 dB
    if fc <= CORNER {
        1.0
    } else {
        (CORNER / fc).powf(EXP).max(FLOOR)
    }
}

/// Transparent soft ceiling: identity below `KNEE`, then a tanh knee that
/// asymptotes to ±1.0 so no material can clip past 0 dBFS. Applied to the wet
/// path of both engines (Classic sits far below the knee, so it's untouched).
#[inline]
fn soft_ceiling(x: f32) -> f32 {
    const KNEE: f32 = 0.7;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        let over = (a - KNEE) / (1.0 - KNEE);
        x.signum() * (KNEE + (1.0 - KNEE) * over.tanh())
    }
}

/// Per-frame spectral cross-synthesis (envelope-transfer form). We keep the
/// carrier's own magnitude **and phase** (its harmonic fine-structure and
/// sparsity — near-zero bins between harmonics stay near-zero) and only swap
/// its spectral *envelope* for the modulator's, via the smoothed ratio
/// `mod_env / car_env`. Because the carrier phase is preserved verbatim, the
/// output is phase-coherent by construction — the identity-phase-lock limit of
/// a Laroche-Dolson vocoder — so there is no phase-vocoder phasiness/metallic
/// beating to correct. The earlier unit-magnitude form flattened the carrier
/// (filled every bin to unity → dense/buzzy) and scattered phase with noise;
/// both were the "harsh/metallic" the envelope-transfer form removes.
/// Modulator envelope for the current hop: smoothed magnitude, then an
/// asymmetric temporal smoother applied to the HF bins (slow attack / fast
/// release, weighted up with frequency) so a sudden onset doesn't spike the top
/// into a hard, sharp transient edge. Runs **once per hop** — the modulator is
/// mono, shared by both output channels — writing the shared `mod_env`.
#[inline]
fn compute_mod_env(
    mod_spec: &[Complex<f32>],
    mag: &mut [f32],
    mod_env: &mut [f32],
    env_smooth: &mut [f32],
    hf_weight: &[f32],
    half: usize,
    smooth_w: usize,
) {
    let w = smooth_w.max(1);
    for k in 0..=half {
        let c = mod_spec[k];
        mag[k] = (c.re * c.re + c.im * c.im).sqrt();
    }
    smooth_env(mag, mod_env, w, half);
    for k in 0..=half {
        let target = mod_env[k];
        let cur = env_smooth[k];
        let a = if target > cur { TSMOOTH_ATTACK } else { TSMOOTH_RELEASE };
        let sm = cur * a + target * (1.0 - a);
        env_smooth[k] = sm;
        let hfw = hf_weight[k];
        mod_env[k] = target * (1.0 - hfw) + sm * hfw;
    }
}

/// Impose the shared `mod_env` (formant-shifted) onto one carrier channel,
/// keeping the carrier's phase + fine-structure (the envelope-transfer form).
/// Adds a soft, frequency-darkened breath for unvoiced consonants and a gentle
/// high shelf so the top matches the Classic band bank's natural roll-off.
#[allow(clippy::too_many_arguments)]
fn spectral_synth(
    car_spec: &[Complex<f32>],
    syn: &mut [Complex<f32>],
    mod_env: &[f32],
    car_env: &mut [f32],
    mag: &mut [f32],
    beta: f32,
    unvoiced: f32,
    noise: &mut u32,
    shelf: &[f32],
    noise_dark: &[f32],
    noise_gate: &[f32],
    half: usize,
    smooth_w: usize,
) {
    let w = smooth_w.max(1);
    // Smoothed carrier envelope (divided out so we keep only its fine-structure).
    for k in 0..=half {
        let c = car_spec[k];
        mag[k] = (c.re * c.re + c.im * c.im).sqrt();
    }
    smooth_env(mag, car_env, w, half);

    for k in 0..=half {
        let src = (k as f32 / beta).round() as usize;
        let me = if src <= half { mod_env[src] } else { 0.0 };
        let ce = car_env[k].max(1e-6);

        let gain = (me / ce).min(SPEC_GAIN_CEIL);

        // Tonal part: carrier fine-structure + phase, re-enveloped.
        let mut s = car_spec[k] * gain;

        // Unvoiced breath — low weight and frequency-darkened so the top isn't a
        // hard hiss; scaled by the local envelope so it reads as consonants.
        // `noise_gate`/`noise_dark` are precomputed; the multiply order matches
        // the original exactly to stay bit-identical.
        let nw = noise_gate[k] * unvoiced * SPEC_NOISE_W;
        if nw > 1e-4 {
            let rp = xorshift_noise(noise) * core::f32::consts::PI;
            let ne = me * nw * noise_dark[k];
            s += Complex::new(rp.cos() * ne, rp.sin() * ne);
        }

        // Gentle high shelf → match the Classic bank's natural top roll-off.
        syn[k] = s * shelf[k];
    }
}

/// Moving-average smoother (half-width `w`) from `src` magnitudes into `dst`.
#[inline]
fn smooth_env(src: &[f32], dst: &mut [f32], w: usize, half: usize) {
    for k in 0..=half {
        let lo = k.saturating_sub(w);
        let hi = (k + w).min(half);
        let mut s = 0.0f32;
        for j in lo..=hi {
            s += src[j];
        }
        dst[k] = s / (hi - lo + 1) as f32;
    }
}

/// Spectral (FFT cross-synthesis) vocoder engine — self-contained so the outer
/// vocoder can dispatch to it cleanly. Stereo = two `StftProcessor`s sharing the
/// mono modulator input, each taking one carrier channel.
pub struct SpectralVocoder {
    stft: [StftProcessor; 2],
    mag: Box<[f32]>,
    /// Smoothed modulator (formant) envelope — also what the GUI curve shows.
    env: Box<[f32]>,
    /// Smoothed carrier envelope, divided out during cross-synthesis.
    car_env: Box<[f32]>,
    /// Temporally-smoothed HF envelope state (attack-softening across hops).
    env_smooth: Box<[f32]>,
    /// Per-bin constants precomputed once in `new()` (a function of the bin
    /// frequency only) so the per-hop loop never re-evaluates `powf`/divides:
    /// `shelf` = `hf_shelf(fc)`, `hf_weight` = the temporal-smoothing HF blend,
    /// `noise_dark` = the breath HF darkening, `noise_gate` = the >4 kHz breath
    /// gate. Kept in the exact operation order of the original so the output
    /// stays bit-identical.
    shelf: Box<[f32]>,
    hf_weight: Box<[f32]>,
    noise_dark: Box<[f32]>,
    noise_gate: Box<[f32]>,
    noise: u32,
    half: usize,
    freq_per_bin: f32,
}

impl SpectralVocoder {
    pub fn new(sr: f32) -> Self {
        // L takes [modulator, carrier_L] (the modulator is FFT'd here, once per
        // hop); R takes only [carrier_R] and reuses L's modulator envelope — so
        // the mono modulator is never FFT'd twice (saves one N-point FFT/hop).
        let stft0 = StftProcessor::new(sr, STFT_N, STFT_HOP, 2);
        let half = stft0.half();
        let freq_per_bin = stft0.freq_per_bin();
        // Precompute the per-bin frequency-dependent constants (identical values
        // to the old inline computations, just hoisted out of the per-hop loop).
        let mut shelf = vec![0.0f32; half + 1].into_boxed_slice();
        let mut hf_weight = vec![0.0f32; half + 1].into_boxed_slice();
        let mut noise_dark = vec![0.0f32; half + 1].into_boxed_slice();
        let mut noise_gate = vec![0.0f32; half + 1].into_boxed_slice();
        for k in 0..=half {
            let fc = k as f32 * freq_per_bin;
            shelf[k] = hf_shelf(fc);
            hf_weight[k] = ((fc - TSMOOTH_LO_HZ) / (TSMOOTH_HI_HZ - TSMOOTH_LO_HZ)).clamp(0.0, 1.0);
            noise_dark[k] = 1.0 / (1.0 + (fc - 5000.0).max(0.0) / 3000.0);
            noise_gate[k] = ((fc - 4000.0) / 4000.0).clamp(0.0, 1.0);
        }
        Self {
            stft: [stft0, StftProcessor::new(sr, STFT_N, STFT_HOP, 1)],
            mag: vec![0.0; half + 1].into_boxed_slice(),
            env: vec![0.0; half + 1].into_boxed_slice(),
            car_env: vec![0.0; half + 1].into_boxed_slice(),
            env_smooth: vec![0.0; half + 1].into_boxed_slice(),
            shelf,
            hf_weight,
            noise_dark,
            noise_gate,
            noise: 0x5eed_1234,
            half,
            freq_per_bin,
        }
    }

    pub fn latency(&self) -> usize {
        self.stft[0].latency()
    }

    /// Log-frequency resample of the current formant envelope (last processed
    /// frame) into `out`, spanning ≈60 Hz…8 kHz — display-ready for the GUI.
    pub fn write_env_curve(&self, out: &mut [f32]) {
        let n = out.len();
        if n == 0 {
            return;
        }
        const LO: f32 = 60.0;
        const HI: f32 = 8000.0;
        let ratio = HI / LO;
        for (j, o) in out.iter_mut().enumerate() {
            let t = if n > 1 { j as f32 / (n as f32 - 1.0) } else { 0.0 };
            let f = LO * ratio.powf(t);
            let bin = (f / self.freq_per_bin).round() as usize;
            *o = if bin <= self.half { self.env[bin] } else { 0.0 };
        }
    }

    /// One stereo sample: mono modulator `m`, stereo carrier, returns the
    /// (STFT-latency-delayed) vocoded stereo pair.
    pub fn process_sample(
        &mut self,
        m: f32,
        car_l: f32,
        car_r: f32,
        formant_semi: f32,
        unvoiced: f32,
        smooth_w: usize,
    ) -> (f32, f32) {
        let beta = 2f32.powf(formant_semi / 12.0);
        let SpectralVocoder {
            stft,
            mag,
            env,
            car_env,
            env_smooth,
            shelf,
            hf_weight,
            noise_dark,
            noise_gate,
            noise,
            half,
            ..
        } = self;
        let half = *half;
        // L pass: compute the shared modulator envelope (mono → once per hop),
        // then synth the left carrier.
        let wl = stft[0].process_sample(&[m, car_l], |ana, syn| {
            compute_mod_env(ana[0], mag, env, env_smooth, hf_weight, half, smooth_w);
            spectral_synth(
                ana[1], syn, env, car_env, mag, beta, unvoiced, noise, shelf, noise_dark,
                noise_gate, half, smooth_w,
            );
        });
        // R pass: carrier only (inputs=1) — reuse the modulator envelope from
        // this hop's L pass, so the modulator isn't FFT'd a second time.
        let wr = stft[1].process_sample(&[car_r], |ana, syn| {
            spectral_synth(
                ana[0], syn, env, car_env, mag, beta, unvoiced, noise, shelf, noise_dark,
                noise_gate, half, smooth_w,
            );
        });
        (wl, wr)
    }
}

/// Maximum band count — the filter banks are stack-allocated at this size and
/// only the first `active_bands` are used, so the `Band Count` switch never
/// touches the allocator on the audio thread.
pub const MAX_BANDS: usize = 20;

/// Polyphony of the internal carrier — how many MIDI notes can sound at once.
/// Enough for lush vocoder chords (Herbie Hancock / Daft Punk) while staying
/// cheap. Pool is fixed so nothing allocates on the audio thread.
pub const MAX_VOICES: usize = 6;

/// `Pitch Source` enum values. Auto = MIDI notes when any key is held, else
/// YIN off the voice; MIDI = keys only (carrier silent with no keys, like a
/// real hardware vocoder); Voice = YIN pitch-tracking only.
pub const PITCH_AUTO: u32 = 0;
pub const PITCH_MIDI: u32 = 1;
pub const PITCH_VOICE: u32 = 2;

/// Band-bank frequency range. 80 Hz captures the fundamental region, 8 kHz the
/// sibilant edge; formants live in between.
const F_LO: f32 = 80.0;
const F_HI: f32 = 8000.0;

/// Carrier-source enum values (the `Carrier Source` param).
pub const SRC_INTERNAL: u32 = 0;
pub const SRC_SIDECHAIN: u32 = 1;

/// Carrier-wave enum values (the `Carrier Wave` param).
pub const WAVE_SAW: u32 = 0;
pub const WAVE_SQUARE: u32 = 1;
pub const WAVE_PULSE: u32 = 2;
pub const WAVE_SAWSUB: u32 = 3;

/// `Band Count` enum values → actual band counts. 11 = the old tinny robot
/// (DigiTech Talker, "Harder Better Faster Stronger"), 16 = balanced default,
/// 20 = the more intelligible modern "R.A.M." sound.
pub const BANDS_11: u32 = 0;
pub const BANDS_16: u32 = 1;
pub const BANDS_20: u32 = 2;

/// Resolve the `Band Count` enum value to an actual band count.
pub fn band_count_from_param(v: u32) -> usize {
    match v {
        BANDS_11 => 11,
        BANDS_20 => 20,
        _ => 16,
    }
}

/// `Detail` enum values (Spectral mode only) — the formant-envelope resolution.
/// Low = broad, classic-vocoder formants; Ultra = fine, near-full FFT detail.
pub const DETAIL_LOW: u32 = 0;
pub const DETAIL_MID: u32 = 1;
pub const DETAIL_HIGH: u32 = 2;
pub const DETAIL_ULTRA: u32 = 3;

/// Map the `Detail` enum to the envelope-smoothing half-width (bins). Bigger =
/// smoother/broader (fewer effective formant bands); smaller = finer. Ranges far
/// past the Classic 11/16/20: Ultra ≈ 128 effective bins vs Low ≈ 11.
pub fn detail_smooth_w(detail: u32) -> usize {
    match detail {
        DETAIL_LOW => 48,
        DETAIL_HIGH => 16,
        DETAIL_ULTRA => 8,
        _ => 28, // Mid (default) — smoother than the old 16-band setting.
    }
}

/// Base makeup gain on the summed vocoded signal (per channel). Each 4th-order
/// band passes only a slice of the carrier and the envelope under-reads a
/// broadband modulator, so the raw sum sits well below unity. Scaled by band
/// count at runtime so loudness stays roughly constant across 11 / 16 / 20.
const VOC_MAKEUP: f32 = 2.2;

/// Makeup for Spectral (envelope-transfer) mode. The output level tracks the
/// modulator's spectral-envelope magnitude, on a completely different scale from
/// the old unit-magnitude form, so this is recalibrated to level-match the
/// Classic band bank on a voice+saw scene (Classic↔Spectral within ~2 dB, no
/// jump when switching Mode). Locked by `classic_spectral_level_match`.
const SPECTRAL_MAKEUP: f32 = 0.28;

/// Length of the carrier-pitch median window (in YIN hops). A harmonic-rich,
/// plucky source (jaw-harp) makes YIN throw a single-hop octave error right on
/// each attack transient; a median over an odd number of recent estimates
/// rejects that spike while still following real drift. 5 hops ≈ 53 ms.
const PITCH_MED: usize = 5;

/// Below this the carrier stays fully tonal; above `NOISE_HI` it can be fully
/// noise (scaled by `Unvoiced Mix`). Sibilants live in this region.
const NOISE_LO_HZ: f32 = 3000.0;
const NOISE_HI_HZ: f32 = 5000.0;

// ---------------------------------------------------------------------------
// Mel-spaced band geometry.
//
//   mel(f)   = 2595 · log10(1 + f/700)
//   mel⁻¹(m) = 700 · (10^(m/2595) − 1)
//
// Spacing centres linearly in mel and mapping back to Hz gives the
// perceptually-denser low/mid coverage a vocoder wants.
// ---------------------------------------------------------------------------

#[inline]
fn hz_to_mel(f: f32) -> f32 {
    2595.0 * (1.0 + f / 700.0).log10()
}

#[inline]
fn mel_to_hz(m: f32) -> f32 {
    700.0 * (10f32.powf(m / 2595.0) - 1.0)
}

/// Centre frequency of band `i` when the bank has `count` bands, mel-spaced
/// across F_LO..F_HI.
pub fn band_center_hz(i: usize, count: usize) -> f32 {
    let t = if count > 1 {
        i as f32 / (count as f32 - 1.0)
    } else {
        0.0
    };
    let m = hz_to_mel(F_LO) + (hz_to_mel(F_HI) - hz_to_mel(F_LO)) * t;
    mel_to_hz(m)
}

// ---------------------------------------------------------------------------
// Band-limited carrier oscillators (PolyBLEP).
// ---------------------------------------------------------------------------

#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if dt <= 0.0 {
        return 0.0;
    }
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

#[inline]
fn poly_saw(phase: f32, dt: f32) -> f32 {
    (2.0 * phase - 1.0) - poly_blep(phase, dt)
}

#[inline]
fn poly_square(phase: f32, dt: f32, duty: f32) -> f32 {
    let mut y = if phase < duty { 1.0 } else { -1.0 };
    y += poly_blep(phase, dt);
    let mut t2 = phase + (1.0 - duty);
    if t2 >= 1.0 {
        t2 -= 1.0;
    }
    y -= poly_blep(t2, dt);
    y
}

#[inline]
fn advance_phase(phase: &mut f32, dt: f32) {
    *phase += dt;
    while *phase >= 1.0 {
        *phase -= 1.0;
    }
}

/// Two detuned band-limited oscillators panned L/R for stereo width, plus a
/// centred octave-down saw for the Saw+Sub wave.
#[derive(Default, Clone, Copy)]
pub struct Carrier {
    p1: f32,
    p2: f32,
    psub: f32,
}

impl Carrier {
    #[inline]
    fn wave_pair(&mut self, wave: u32, dt1: f32, dt2: f32) -> (f32, f32) {
        let out = match wave {
            WAVE_SQUARE => (poly_square(self.p1, dt1, 0.5), poly_square(self.p2, dt2, 0.5)),
            WAVE_PULSE => (poly_square(self.p1, dt1, 0.30), poly_square(self.p2, dt2, 0.30)),
            _ => (poly_saw(self.p1, dt1), poly_saw(self.p2, dt2)),
        };
        advance_phase(&mut self.p1, dt1);
        advance_phase(&mut self.p2, dt2);
        out
    }

    /// One stereo carrier sample at `base_hz`. The detune split (`half`), the
    /// pan weights (`a`, `b`) and the `norm` are all functions of the per-block
    /// `detune_cents` / `wave`, so the caller computes them once per block via
    /// [`Carrier::detune_pan`] and passes them in — only the phase advance and
    /// `base_hz`-dependent frequencies stay per-sample. `wave` is one of the
    /// `WAVE_*` constants.
    #[allow(clippy::too_many_arguments)]
    pub fn next_stereo(
        &mut self,
        wave: u32,
        base_hz: f32,
        half: f32,
        a: f32,
        b: f32,
        norm: f32,
        sr: f32,
    ) -> (f32, f32) {
        let base_hz = base_hz.clamp(20.0, sr * 0.45);
        // Split the detune symmetrically: voice 1 down, voice 2 up.
        let f1 = base_hz / half;
        let f2 = base_hz * half;
        let dt1 = (f1 / sr).min(0.49);
        let dt2 = (f2 / sr).min(0.49);

        let (v1, v2) = self.wave_pair(wave, dt1, dt2);

        // Centred sub octave for Saw+Sub (kept mono so the low end stays solid).
        let sub = if wave == WAVE_SAWSUB {
            let dts = (base_hz * 0.5 / sr).min(0.49);
            let s = poly_saw(self.psub, dts);
            advance_phase(&mut self.psub, dts);
            0.6 * s
        } else {
            0.0
        };

        let l = (v1 * a + v2 * b + sub) * norm;
        let r = (v2 * a + v1 * b + sub) * norm;
        (l, r)
    }

    /// Per-block detune/pan constants for [`next_stereo`] — `(half, a, b, norm)`.
    /// Depends only on `detune_cents` and `wave`, both per-block parameters, so
    /// hoisting this out of the per-sample loop is bit-identical.
    #[inline]
    pub fn detune_pan(detune_cents: f32, wave: u32) -> (f32, f32, f32, f32) {
        let half = 2f32.powf(detune_cents / 2400.0);
        // More detune → wider stereo image (voice 1 leans L, voice 2 leans R).
        let spread = (detune_cents / 25.0).clamp(0.0, 1.0) * 0.85;
        let a = 0.5 * (1.0 + spread);
        let b = 0.5 * (1.0 - spread);
        let norm = if wave == WAVE_SAWSUB { 1.0 / 1.6 } else { 1.0 };
        (half, a, b, norm)
    }
}

/// xorshift32 → white noise in [-1, 1]. Alloc-free, RT-safe.
#[inline]
fn xorshift_noise(state: &mut u32) -> f32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    (x as f32 / u32::MAX as f32) * 2.0 - 1.0
}

// ---------------------------------------------------------------------------
// Per-block parameter snapshot passed into `process_stereo`.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct VocParams {
    pub attack_ms: f32,
    pub release_ms: f32,
    pub source: u32,
    pub wave: u32,
    /// Number of active bands (11 / 16 / 20 — already resolved from the enum).
    pub band_count: usize,
    /// Where the internal carrier gets its pitch (`PITCH_*`).
    pub pitch_source: u32,
    /// Held MIDI notes, one per voice slot; `-1` = empty. Allocated by the
    /// CLAP layer from NoteOn/NoteOff; used when the pitch source resolves to
    /// MIDI.
    pub notes: [i16; MAX_VOICES],
    pub pitch_offset_semi: f32,
    pub detune_cents: f32,
    pub formant_semi: f32,
    /// Noise-excitation amount for the upper bands, 0..1.
    pub unvoiced: f32,
    /// Carrier drive, 0..1.
    pub drive: f32,
    /// Dry/Wet, 0..1 (1 = fully vocoded).
    pub mix: f32,
    /// Output trim as a linear gain.
    pub output_lin: f32,
    /// Engine: `MODE_CLASSIC` (band bank) or `MODE_SPECTRAL` (FFT cross-synth).
    pub mode: u32,
    /// Spectral-only: formant-envelope resolution (`DETAIL_*`). Ignored in Classic.
    pub detail: u32,
    pub bypassed: bool,
}

// ---------------------------------------------------------------------------
// The vocoder.
// ---------------------------------------------------------------------------

pub struct Vocoder {
    sr: f32,
    centers: [f32; MAX_BANDS],
    /// Precomputed per-band unvoiced-noise crossfade weight (a function of the
    /// band centre); rebuilt with the banks, read per-sample × `unvoiced`.
    band_noise_weight: [f32; MAX_BANDS],
    /// Analysis bank — two cascaded BPFs per band (mono modulator path).
    ana_a: [Biquad; MAX_BANDS],
    ana_b: [Biquad; MAX_BANDS],
    /// Per-band modulator envelope follower.
    env: [EnvelopeDetector; MAX_BANDS],
    /// Synthesis banks — two cascaded BPFs per band, one bank per channel for
    /// the stereo carrier. Re-tuned when Formant Shift or Band Count changes.
    syn_a_l: [Biquad; MAX_BANDS],
    syn_b_l: [Biquad; MAX_BANDS],
    syn_a_r: [Biquad; MAX_BANDS],
    syn_b_r: [Biquad; MAX_BANDS],
    /// Polyphonic carrier oscillator pool (one per MIDI voice; the YIN path
    /// uses slot 0). Each is a stereo detuned voice.
    carriers: [Carrier; MAX_VOICES],
    /// Per-voice gate gain — one-pole ramp so notes don't click on / off.
    voice_gain: [f32; MAX_VOICES],
    /// Per-voice current frequency (held during the release ramp).
    voice_freq: [f32; MAX_VOICES],
    tracker: YinPitchTracker,
    noise_l: u32,
    noise_r: u32,
    active_bands: usize,
    last_formant: f32,
    sm_mix: SmoothedParam,
    sm_unvoiced: SmoothedParam,
    sm_drive: SmoothedParam,
    sm_output: SmoothedParam,
    /// Carrier-pitch stabiliser. `pitch_hist` is a ring of the last `PITCH_MED`
    /// YIN estimates (median kills pluck-onset octave glitches); `pitch_glide`
    /// portamentos toward the median target so a pitch change is a glide, never
    /// a stepped squeak.
    pitch_hist: [f32; PITCH_MED],
    pitch_hist_i: usize,
    /// Cached median of `pitch_hist` — recomputed only when a new YIN estimate
    /// lands (not per sample); the glide reads it each sample.
    pitch_median: f32,
    pitch_glide: f32,
    /// Spectral (FFT cross-synthesis) engine — used in `Mode::Spectral`.
    spectral: SpectralVocoder,
    /// Latency alignment so both modes report the same host latency
    /// (STFT_LATENCY): the dry always delayed by it, the Classic wet delayed by
    /// it (Spectral wet is already delayed inside the StftProcessor).
    dry_delay: [LatencyDelay; 2],
    wet_cl_delay: [LatencyDelay; 2],
    /// Last-block per-band envelope levels for the Classic activity meter.
    viz_bars: [f32; MAX_BANDS],
}

impl Vocoder {
    pub fn new(sr: f32) -> Self {
        let dl = || LatencyDelay::new(STFT_LATENCY);
        let mut me = Self {
            sr,
            centers: [0.0; MAX_BANDS],
            band_noise_weight: [0.0; MAX_BANDS],
            ana_a: [Biquad::default(); MAX_BANDS],
            ana_b: [Biquad::default(); MAX_BANDS],
            env: [EnvelopeDetector::default(); MAX_BANDS],
            syn_a_l: [Biquad::default(); MAX_BANDS],
            syn_b_l: [Biquad::default(); MAX_BANDS],
            syn_a_r: [Biquad::default(); MAX_BANDS],
            syn_b_r: [Biquad::default(); MAX_BANDS],
            carriers: [Carrier::default(); MAX_VOICES],
            voice_gain: [0.0; MAX_VOICES],
            voice_freq: [110.0; MAX_VOICES],
            // Range tuned for voice + bass instruments: min 55 Hz so a jaw-harp
            // fundamental (~73 Hz) is representable (the old 75 Hz floor forced
            // YIN onto the 2nd harmonic → carrier jumped an octave up), max
            // 600 Hz so it can't chase a mid-band harmonic into the squeaky
            // register. Window 2048 covers two periods of 55 Hz.
            tracker: YinPitchTracker::new(sr, 55.0, 600.0, 2048, 512, 110.0),
            noise_l: 0x1234_5678,
            noise_r: 0x9E37_79B9,
            active_bands: 16,
            last_formant: f32::NAN,
            sm_mix: SmoothedParam::new(1.0),
            sm_unvoiced: SmoothedParam::new(0.15),
            sm_drive: SmoothedParam::new(0.0),
            sm_output: SmoothedParam::new(1.0),
            pitch_hist: [110.0; PITCH_MED],
            pitch_hist_i: 0,
            pitch_median: 110.0,
            pitch_glide: 110.0,
            spectral: SpectralVocoder::new(sr),
            dry_delay: [dl(), dl()],
            wet_cl_delay: [dl(), dl()],
            viz_bars: [0.0; MAX_BANDS],
        };
        me.rebuild_banks(16, 0.0);
        me
    }

    /// Reported latency (samples) — same for both modes so switching Mode never
    /// re-triggers host PDC.
    pub fn latency_samples(&self) -> u32 {
        STFT_LATENCY as u32
    }

    /// Snap the smoothers to the host-loaded initial values so the first block
    /// doesn't glide up from a default.
    pub fn prime(&mut self, mix: f32, unvoiced: f32, drive: f32, output_lin: f32) {
        self.sm_mix.snap(mix);
        self.sm_unvoiced.snap(unvoiced);
        self.sm_drive.snap(drive);
        self.sm_output.snap(output_lin);
    }

    /// Recompute band centres, Qs and every biquad for a given band count +
    /// formant shift. Called off the per-sample path (only when Band Count or
    /// Formant Shift actually changes) so it never runs RBJ design per sample.
    fn rebuild_banks(&mut self, count: usize, formant_semi: f32) {
        let count = count.clamp(2, MAX_BANDS);
        for i in 0..count {
            self.centers[i] = band_center_hz(i, count);
            // Per-band unvoiced-noise crossfade weight — a fixed function of the
            // centre, so precompute it off the per-sample path.
            self.band_noise_weight[i] = ((self.centers[i] - NOISE_LO_HZ)
                / (NOISE_HI_HZ - NOISE_LO_HZ))
                .clamp(0.0, 1.0);
        }
        // Per-band Q from geometric neighbour spacing so bands roughly tile the
        // spectrum. Clamped to keep the corner bands sane. Scratch-only → local.
        let mut q = [0.0f32; MAX_BANDS];
        for i in 0..count {
            let lo = if i == 0 {
                self.centers[0] * self.centers[0] / self.centers[1]
            } else {
                (self.centers[i - 1] * self.centers[i]).sqrt()
            };
            let hi = if i == count - 1 {
                self.centers[i] * self.centers[i] / self.centers[i - 1]
            } else {
                (self.centers[i] * self.centers[i + 1]).sqrt()
            };
            let bw = (hi - lo).max(1.0);
            q[i] = (self.centers[i] / bw).clamp(1.5, 9.0);
        }

        let mult = 2f32.powf(formant_semi / 12.0);
        let sr = self.sr;
        for i in 0..count {
            let fa = self.centers[i].clamp(20.0, sr * 0.45);
            self.ana_a[i].set_bandpass(sr, fa, q[i]);
            self.ana_b[i].set_bandpass(sr, fa, q[i]);
            let fs = (self.centers[i] * mult).clamp(20.0, sr * 0.45);
            self.syn_a_l[i].set_bandpass(sr, fs, q[i]);
            self.syn_b_l[i].set_bandpass(sr, fs, q[i]);
            self.syn_a_r[i].set_bandpass(sr, fs, q[i]);
            self.syn_b_r[i].set_bandpass(sr, fs, q[i]);
        }
        self.active_bands = count;
        self.last_formant = formant_semi;
    }

    /// Process one stereo block: read from `in_*`, write to `out_*`. `sc_*` is
    /// the sidechain carrier (silent slices are fine). For mono input, pass the
    /// same slice for L and R and ignore `out_r`.
    #[allow(clippy::too_many_arguments)]
    pub fn process_stereo(
        &mut self,
        in_l: &[f32],
        in_r: &[f32],
        out_l: &mut [f32],
        out_r: &mut [f32],
        sc_l: &[f32],
        sc_r: &[f32],
        p: &VocParams,
    ) {
        let n = in_l.len().min(out_l.len());

        if p.bypassed {
            out_l[..n].copy_from_slice(&in_l[..n]);
            let rn = n.min(in_r.len()).min(out_r.len());
            out_r[..rn].copy_from_slice(&in_r[..rn]);
            return;
        }

        // Re-tune the banks only when Band Count or Formant Shift moves.
        let want_bands = p.band_count.clamp(2, MAX_BANDS);
        if want_bands != self.active_bands
            || !(p.formant_semi - self.last_formant).abs().is_finite()
            || (p.formant_semi - self.last_formant).abs() > 0.01
        {
            self.rebuild_banks(want_bands, p.formant_semi);
        }

        let sr = self.sr;
        let active = self.active_bands;
        let pitch_mult = 2f32.powf(p.pitch_offset_semi / 12.0);
        let internal = p.source == SRC_INTERNAL;
        // Keep robot-voice loudness roughly constant across band counts.
        // More bands => narrower constant-Q filters => each captures/reconstructs
        // less energy, so the summed output gets *quieter* as bands increase.
        // Compensation must therefore scale UP with band count (linear in
        // `active` matches measured RMS across 11/16/20 to within ~0.3 dB — an
        // earlier `sqrt(16/active)` scaled the wrong way and left 20-band ~5 dB
        // quiet). 16 bands stays at the VOC_MAKEUP reference.
        let makeup = VOC_MAKEUP * (active as f32 / 16.0);
        // Spectral mode: FFT cross-synthesis instead of the band bank.
        let spectral = p.mode == MODE_SPECTRAL;
        // Envelope smoothing width for Spectral — driven by the `Detail` control
        // (independent of Classic's band count), wider than 11/16/20 bands.
        let smooth_w = detail_smooth_w(p.detail);
        // ~5 ms one-pole gate ramp so MIDI note on/off is click-free.
        let gate_coef = 1.0 - (-1.0 / (0.005 * sr)).exp();
        // ~40 ms portamento glide coefficient (sr constant → hoisted off the loop).
        let pcoef = 1.0 - (-1.0 / (0.040 * sr)).exp();
        // Resolve the pitch source once per block. Sidechain ignores MIDI.
        let any_note = p.notes.iter().any(|&nn| nn >= 0);
        let use_midi = internal
            && match p.pitch_source {
                PITCH_MIDI => true,
                PITCH_VOICE => false,
                _ => any_note, // Auto
            };
        let n_on = if use_midi {
            p.notes.iter().filter(|&&nn| nn >= 0).count().max(1)
        } else {
            1
        };
        let voice_norm = 1.0 / (n_on as f32).sqrt();
        // Carrier detune split + pan weights — functions of the per-block
        // `detune_cents`/`wave`, so computed once here (hoisted off the per-voice
        // per-sample path; bit-identical).
        let (car_half, car_a, car_b, car_norm) = Carrier::detune_pan(p.detune_cents, p.wave);

        // Per-band activity meter (Classic viz) — block-peak of each band's
        // envelope. Reset per block; published to the lock-free snapshot after.
        let mut viz_bar = [0.0f32; MAX_BANDS];

        for i in 0..n {
            let xl = in_l[i];
            let xr = *in_r.get(i).unwrap_or(&xl);
            let m = (xl + xr) * 0.5;

            // ---- Carrier (stereo, polyphonic) ------------------------------
            let (car_l, car_r) = if internal {
                // Keep YIN warm even in MIDI mode so Auto can switch instantly.
                if self.tracker.push(m) {
                    // Fresh estimate this hop → into the median ring, then
                    // recompute the median once (rejects the single-hop octave
                    // error a pluck transient throws). Cheap: only on a new hop.
                    self.pitch_hist[self.pitch_hist_i] = self.tracker.current_hz();
                    self.pitch_hist_i = (self.pitch_hist_i + 1) % PITCH_MED;
                    let mut tmp = self.pitch_hist;
                    self.pitch_median = median_small(&mut tmp);
                }
                // Portamento glide turns any real pitch move into a slide
                // instead of a stepped squeak.
                let target = (self.pitch_median * pitch_mult).clamp(20.0, sr * 0.45);
                self.pitch_glide += (target - self.pitch_glide) * pcoef;
                let yin_hz = self.pitch_glide;

                let mut cl = 0.0f32;
                let mut cr = 0.0f32;
                for k in 0..MAX_VOICES {
                    let (target_gain, target_freq) = if use_midi {
                        if p.notes[k] >= 0 {
                            let f = (midi_note_to_hz(p.notes[k] as f32) * pitch_mult)
                                .clamp(20.0, sr * 0.45);
                            (1.0, f)
                        } else {
                            (0.0, self.voice_freq[k])
                        }
                    } else if k == 0 {
                        (1.0, yin_hz)
                    } else {
                        (0.0, self.voice_freq[k])
                    };
                    if target_gain > 0.0 {
                        self.voice_freq[k] = target_freq;
                    }
                    self.voice_gain[k] += (target_gain - self.voice_gain[k]) * gate_coef;
                    if self.voice_gain[k] > 1e-4 {
                        let (l, r) = self.carriers[k].next_stereo(
                            p.wave,
                            self.voice_freq[k],
                            car_half,
                            car_a,
                            car_b,
                            car_norm,
                            sr,
                        );
                        cl += l * self.voice_gain[k];
                        cr += r * self.voice_gain[k];
                    }
                }
                (cl * voice_norm, cr * voice_norm)
            } else {
                let s = (*sc_l.get(i).unwrap_or(&0.0) + *sc_r.get(i).unwrap_or(&0.0)) * 0.5;
                (s, s)
            };

            let unvoiced = self.sm_unvoiced.step(p.unvoiced, sr);
            let drive_amt = self.sm_drive.step(p.drive, sr);
            let mix = self.sm_mix.step(p.mix, sr);
            let out_lin = self.sm_output.step(p.output_lin, sr);

            // ---- Dry, delayed by STFT_LATENCY so both modes align to the same
            //      reported host latency -------------------------------------
            let dry_l = self.dry_delay[0].process(xl);
            let dry_r = self.dry_delay[1].process(xr);

            // ---- Wet: Classic band bank or Spectral cross-synthesis --------
            let (raw_l, raw_r) = if spectral {
                let (sl, sr_) =
                    self.spectral.process_sample(m, car_l, car_r, p.formant_semi, unvoiced, smooth_w);
                (sl * SPECTRAL_MAKEUP, sr_ * SPECTRAL_MAKEUP)
            } else {
                let noise_l = xorshift_noise(&mut self.noise_l);
                let noise_r = xorshift_noise(&mut self.noise_r);
                let mut voc_l = 0.0f32;
                let mut voc_r = 0.0f32;
                for b in 0..active {
                    let band_mod = self.ana_b[b].process(self.ana_a[b].process(m));
                    let amp = self.env[b].process(band_mod, sr, p.attack_ms, p.release_ms);
                    // Block-peak-hold the per-band envelope for the activity display.
                    viz_bar[b] = viz_bar[b].max(amp);
                    // Upper (sibilant) bands cross-fade the carrier toward noise
                    // (weight precomputed per band in `rebuild_banks`).
                    let nw = self.band_noise_weight[b] * unvoiced;
                    let src_l = car_l * (1.0 - nw) + noise_l * nw;
                    let src_r = car_r * (1.0 - nw) + noise_r * nw;
                    voc_l += self.syn_b_l[b].process(self.syn_a_l[b].process(src_l)) * amp;
                    voc_r += self.syn_b_r[b].process(self.syn_a_r[b].process(src_r)) * amp;
                }
                // Delay the (0-latency) Classic wet to match the reported latency.
                let dwl = self.wet_cl_delay[0].process(voc_l * makeup);
                let dwr = self.wet_cl_delay[1].process(voc_r * makeup);
                (dwl, dwr)
            };

            // ---- Drive (symmetric tanh) + output ---------------------------
            let (mut wet_l, mut wet_r) = (raw_l, raw_r);
            if drive_amt > 1e-4 {
                let drive_lin = 1.0 + drive_amt * 6.0;
                wet_l = tanh_drive(wet_l, drive_lin);
                wet_r = tanh_drive(wet_r, drive_lin);
            }
            if !wet_l.is_finite() {
                wet_l = 0.0;
            }
            if !wet_r.is_finite() {
                wet_r = 0.0;
            }
            // Transparent safety ceiling so no scene can push the wet past 0 dBFS
            // (Spectral cross-synthesis can spike on hot broadband material).
            wet_l = soft_ceiling(wet_l);
            wet_r = soft_ceiling(wet_r);
            out_l[i] = dry_l * (1.0 - mix) + wet_l * mix * out_lin;
            if i < out_r.len() {
                out_r[i] = dry_r * (1.0 - mix) + wet_r * mix * out_lin;
            }
        }

        // Publish the Classic band-activity meter (Spectral publishes its
        // envelope curve via `write_env_curve`, sampled by the caller).
        self.viz_bars = viz_bar;
    }

    /// Last block's per-band envelope levels (Classic activity display). Only
    /// the first `active_bands` are meaningful.
    pub fn viz_bars(&self) -> &[f32] {
        &self.viz_bars
    }

    /// Write the current Spectral formant envelope into `out`, log-frequency
    /// resampled (≈60 Hz…8 kHz) for the activity display.
    pub fn write_env_curve(&self, out: &mut [f32]) {
        self.spectral.write_env_curve(out);
    }
}
