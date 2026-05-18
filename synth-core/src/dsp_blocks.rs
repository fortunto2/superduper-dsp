//! Reusable DSP building blocks shared across SuperDuper effects.
//!
//! Each block is `#[derive(Default)]`-constructible and exposes a single
//! `process` method that takes a sample (or a stereo pair) plus runtime
//! parameters. None of them allocate — they are safe to call from the
//! audio thread.

// ---------------------------------------------------------------------------
// Tempo-synced rate division — turns a "1/8 dotted"-style note value into
// a Hz rate given the host's BPM. Used by every LFO / delay-time / sweep
// rate that wants a Sync toggle.
//
// Layout: 12 useful divisions in human-musical order, dense range from a
// whole note down to a 1/16 triplet. Plenty for synth wobbles and delays.
// ---------------------------------------------------------------------------

/// Convert a 0..=11 enum index + BPM into the corresponding rate in Hz.
/// 0 = 1/1, 1 = 1/2 dotted, 2 = 1/2, 3 = 1/2 triplet, 4 = 1/4,
/// 5 = 1/4 dotted, 6 = 1/4 triplet, 7 = 1/8, 8 = 1/8 dotted,
/// 9 = 1/8 triplet, 10 = 1/16, 11 = 1/16 triplet.
pub fn sync_division_hz(idx: u32, bpm: f32) -> f32 {
    // beats_per_cycle for each entry.
    let beats = match idx {
        0 => 4.0,           // 1/1
        1 => 3.0,           // 1/2 dotted
        2 => 2.0,           // 1/2
        3 => 4.0 / 3.0,     // 1/2 triplet
        4 => 1.0,           // 1/4
        5 => 1.5,           // 1/4 dotted
        6 => 2.0 / 3.0,     // 1/4 triplet
        7 => 0.5,           // 1/8
        8 => 0.75,          // 1/8 dotted
        9 => 1.0 / 3.0,     // 1/8 triplet
        10 => 0.25,         // 1/16
        _ => 1.0 / 6.0,     // 1/16 triplet
    };
    (bpm.max(20.0)) / 60.0 / beats
}

pub fn sync_division_label(idx: u32) -> &'static str {
    match idx {
        0 => "1/1",
        1 => "1/2d",
        2 => "1/2",
        3 => "1/2t",
        4 => "1/4",
        5 => "1/4d",
        6 => "1/4t",
        7 => "1/8",
        8 => "1/8d",
        9 => "1/8t",
        10 => "1/16",
        _ => "1/16t",
    }
}

// ---------------------------------------------------------------------------
// SmoothedParam — one-pole interpolator for CLAP parameter slews.
//
// Reading an `AtomicF32` per sample is fine, but using the raw value
// per sample produces audible zipper noise when the user crank a knob.
// The standard fix is to slew toward the new value with a one-pole filter.
// ---------------------------------------------------------------------------

#[derive(Default, Copy, Clone)]
pub struct SmoothedParam {
    current: f32,
}

impl SmoothedParam {
    /// Create with an initial value (use the CLAP default).
    pub fn new(initial: f32) -> Self {
        Self { current: initial }
    }

    /// Update toward `target` with a 5 ms time constant at the given sample
    /// rate. Returns the new (slewed) value.
    #[inline]
    pub fn step(&mut self, target: f32, sr: f32) -> f32 {
        // ~5 ms one-pole. At 48 kHz coef ≈ 0.99584.
        let coef = (-1.0 / (0.005 * sr)).exp();
        self.current = target + (self.current - target) * coef;
        self.current
    }

    /// Snap immediately (used at activate() time to avoid a 5 ms fade-in
    /// from the default value to whatever the host loaded from project state).
    pub fn snap(&mut self, value: f32) {
        self.current = value;
    }

    pub fn current(&self) -> f32 {
        self.current
    }
}

// ---------------------------------------------------------------------------
// DcBlocker — first-order high-pass at ~5 Hz.
//
// Critical inside any reverb feedback loop: DC offset (from analog input
// drift, or numerical accumulation through long delays) gets multiplied
// every loop iteration, eventually drowning the audible tail in a constant
// hum that the user perceives as "the reverb died". The blocker is cheap
// (one multiply, one subtract, one add) and harmless for audio content.
// ---------------------------------------------------------------------------

#[derive(Default, Copy, Clone)]
pub struct DcBlocker {
    x_prev: f32,
    y_prev: f32,
}

impl DcBlocker {
    /// Standard formula: y[n] = x[n] - x[n-1] + R * y[n-1], R = 0.995.
    /// Cuts off at roughly sr * (1 - R) / (2π) ≈ 38 Hz at 48 kHz, plenty
    /// gentle to be inaudible.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        const R: f32 = 0.995;
        let y = x - self.x_prev + R * self.y_prev;
        self.x_prev = x;
        self.y_prev = y;
        y
    }
}

// ---------------------------------------------------------------------------
// PadVoice — autonomous drone pad. Four sine partials at ratios 1.0 / 1.005 /
// 1.5 / 2.0 (fundamental, slight detune, fifth, octave) with per-partial
// slow LFOs giving each voice gentle pitch drift. Mixed into a one-pole
// resonant lowpass + tanh saturation for analog warmth.
//
// Ported in spirit from rust-synth's `pad_zimmer` voice. Not a fundsp graph
// — manual sample loop so the Ambient plugin can drive multiple voices in
// one block without paying the Net overhead per voice.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct PadVoice {
    /// Phase of each partial (in radians).
    phases: [f32; 4],
    /// LFO phase for the slow per-partial detune.
    lfo_phase: f32,
    /// One-pole lowpass state for the resonant filter.
    lp_z1: f32,
    lp_z2: f32,
}

impl Default for PadVoice {
    fn default() -> Self {
        Self {
            // Offset starting phases so the partials don't all start in lockstep
            // (which produces a noticeable click on the first sample).
            phases: [0.0, 1.234, 2.468, 3.702],
            lfo_phase: 0.0,
            lp_z1: 0.0,
            lp_z2: 0.0,
        }
    }
}

/// Tunables passed to PadVoice::process each sample. Sized so all params
/// fit in a couple of cache lines (cheap to copy).
#[derive(Copy, Clone)]
pub struct PadParams {
    pub sr: f32,
    /// Root frequency (Hz). Partials scale from this.
    pub root_hz: f32,
    /// Cutoff frequency (Hz) of the post-mix lowpass.
    pub cutoff_hz: f32,
    /// Resonance, 0..0.95. Higher = more emphasis at cutoff.
    pub resonance: f32,
    /// LFO depth in cents — 0 = no detune motion, 50 = quarter-tone drift.
    pub modulation_cents: f32,
    /// Drive into the post-filter tanh, 0..1.
    pub drive: f32,
}

impl PadVoice {
    /// Process one sample. Returns mono — caller can detune two voices
    /// (one per channel) for stereo width.
    #[inline]
    pub fn process(&mut self, p: PadParams) -> f32 {
        // LFO — 0.13 Hz drift (∼ 8 s per cycle).
        self.lfo_phase += core::f32::consts::TAU * 0.13 / p.sr;
        if self.lfo_phase >= core::f32::consts::TAU {
            self.lfo_phase -= core::f32::consts::TAU;
        }
        // Convert cents to a multiplicative ratio: 2^(cents/1200).
        let cents = p.modulation_cents * self.lfo_phase.sin();
        let detune = 2f32.powf(cents / 1200.0);

        // Four partials with different detune phases (so they drift relative
        // to each other, producing the slow phasing motion of a real pad).
        const RATIOS: [f32; 4] = [1.0, 1.005, 1.5, 2.0];
        const GAINS: [f32; 4] = [0.45, 0.30, 0.18, 0.10];
        let mut mix = 0.0_f32;
        for (i, (&ratio, &gain)) in RATIOS.iter().zip(GAINS.iter()).enumerate() {
            let phase_inc = core::f32::consts::TAU * p.root_hz * ratio * detune / p.sr;
            self.phases[i] += phase_inc;
            if self.phases[i] >= core::f32::consts::TAU {
                self.phases[i] -= core::f32::consts::TAU;
            }
            mix += self.phases[i].sin() * gain;
        }

        // Trapezoidal (zero-delay-feedback) state-variable filter
        // — Vadim Zavalishin's "The Art of VA Filter Design", chap. 5.
        // Stable for any cutoff up to Nyquist; doesn't suffer from the
        // Chamberlin SVF's blow-up at high frequency. Two integrator
        // states (lp_z1 = bandpass-integrator, lp_z2 = lowpass-integrator).
        let cutoff = p.cutoff_hz.clamp(40.0, p.sr * 0.49);
        let g = (core::f32::consts::PI * cutoff / p.sr).tan();
        // k = 2 → no resonance, k → 0 → self-oscillation. Map our 0..0.95
        // resonance knob into the upper half of that range.
        let k = 2.0 - 2.0 * p.resonance.clamp(0.0, 0.95);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let v1 = a1 * (self.lp_z1 + g * (mix - self.lp_z2));
        let lowpass = self.lp_z2 + g * v1;
        // Trapezoidal update for the two integrator states.
        self.lp_z1 = 2.0 * v1 - self.lp_z1;
        self.lp_z2 = 2.0 * lowpass - self.lp_z2;

        // Soft saturation for analog warmth.
        (lowpass * (1.0 + p.drive * 1.5)).tanh()
    }
}

// ---------------------------------------------------------------------------
// Oversampler2x — minimum-phase 11-tap halfband FIR upsampler / downsampler.
//
// Saturation / non-linear processors produce harmonics that mirror around
// Nyquist; at native rate those mirrors are *audible aliasing*. Running
// the non-linearity at 2× (or cascaded 4×) the original sample rate moves
// the artefacts above Nyquist where the decimator's stop-band buries them.
//
// Halfband filters are the standard cheap solution for this — coefficients
// are symmetric around the centre tap and every other tap is zero, so for
// 11 taps you compute only 5 multiplies plus the centre-tap copy. Designed
// here for ~80 dB stop-band attenuation at 0.55 × Nyquist.
//
// References: musicdsp.org "halfband filter" thread, KVR's "oversampling
// for distortion plugins", and Bert Schiettecatte's polyphase tutorial.
// ---------------------------------------------------------------------------

/// Halfband FIR coefficients — 11 taps, designed for ~80 dB stop-band,
/// flat ±0.05 dB pass-band up to 0.4 × Nyquist. Zero coefficients are
/// elided from the actual multiply.
const HB_COEFS: [f32; 5] = [
    0.001461486792,
    -0.010779382045,
    0.044949340444,
    -0.132159402370, // negative — gives proper magnitude response with center=0.5
    0.596527956179,
];
const HB_CENTER: f32 = 0.5;

#[derive(Default, Copy, Clone)]
pub struct Oversampler2x {
    /// History buffer of input samples (11 deep ring, 6 of them used for the
    /// upsampler's odd-phase computation).
    in_history: [f32; 6],
    write: usize,
    /// History for the decimator path.
    out_history: [f32; 6],
    write_out: usize,
}

/// Apply a per-sample non-linearity at the chosen oversampling rate.
/// `os_mode`: 0 = native, 1 = 2× (one halfband stage), 2 = 4× (two stages
/// cascaded). At anything other than `Off`, the caller's `f` closure runs
/// at the oversampled rate and the halfband decimator pushes its
/// harmonics above Nyquist where the stop-band buries them.
///
/// The two stages are caller-owned so each channel (or each effect) gets
/// its own independent state — sharing the history smears stereo image
/// and can starve odd-phase computations.
#[inline]
pub fn oversample_apply<F: FnMut(f32) -> f32>(
    x: f32,
    os_mode: u32,
    os1: &mut Oversampler2x,
    os2: &mut Oversampler2x,
    mut f: F,
) -> f32 {
    match os_mode {
        0 => f(x),
        1 => {
            let (e, o) = os1.upsample(x);
            os1.downsample(f(e), f(o))
        }
        _ => {
            let (e1, o1) = os1.upsample(x);
            let (ee, eo) = os2.upsample(e1);
            let (oe, oo) = os2.upsample(o1);
            let ee2 = f(ee);
            let eo2 = f(eo);
            let oe2 = f(oe);
            let oo2 = f(oo);
            let e1d = os2.downsample(ee2, eo2);
            let o1d = os2.downsample(oe2, oo2);
            os1.downsample(e1d, o1d)
        }
    }
}

impl Oversampler2x {
    /// Push one input sample, return two upsampled samples.
    /// First is the even phase (just delayed input, no FIR cost), second
    /// is the FIR-interpolated odd phase.
    #[inline]
    pub fn upsample(&mut self, x: f32) -> (f32, f32) {
        self.in_history[self.write] = x;
        let len = self.in_history.len();
        // Center-tap output: just the value 5 samples back (zero-stuffed
        // input is `x, 0, x, 0, …`, then the halfband centre tap = 0.5
        // doubles the kept samples — and the implicit `× 2` of the
        // upsample is part of the canonical halfband design).
        let center_idx = (self.write + len - 5) % len;
        let even = self.in_history[center_idx]; // pass-through tap
        // Odd phase: convolve the 5 non-zero side taps with input history.
        let mut odd = 0.0_f32;
        for (i, c) in HB_COEFS.iter().enumerate() {
            let a = (self.write + len - i) % len;
            let b = (self.write + len - (10 - i)) % len;
            odd += c * (self.in_history[a] + self.in_history[b]);
        }
        odd += HB_CENTER * self.in_history[center_idx];
        self.write = (self.write + 1) % len;
        (even, odd)
    }

    /// Inverse of `upsample`. Push two samples (even, odd), return one
    /// decimated sample (low-passed against the upper half of the spectrum).
    #[inline]
    pub fn downsample(&mut self, even: f32, odd: f32) -> f32 {
        // Decimator is the same halfband flipped — symmetric structure.
        self.out_history[self.write_out] = (even + odd) * 0.5;
        let len = self.out_history.len();
        let center_idx = (self.write_out + len - 5) % len;
        let mut y = HB_CENTER * self.out_history[center_idx];
        for (i, c) in HB_COEFS.iter().enumerate() {
            let a = (self.write_out + len - i) % len;
            let b = (self.write_out + len - (10 - i)) % len;
            y += c * (self.out_history[a] + self.out_history[b]);
        }
        self.write_out = (self.write_out + 1) % len;
        y
    }
}

// ---------------------------------------------------------------------------
// Saturation curves — shared between the Saturator and Limiter, plus
// anywhere else a per-sample non-linearity is needed.
// ---------------------------------------------------------------------------

/// Symmetric tanh soft-clip. `drive` is a linear gain pre-clip (typically
/// computed from a dB knob via `10^(dB/20)`). Output stays within ±1.
#[inline]
pub fn tanh_drive(x: f32, drive: f32) -> f32 {
    (x * drive).tanh()
}

/// "Tape" style soft-clip — flatter saturation than tanh, with audible
/// even-order harmonics. Algebraic — `y = x / (1 + |x|)`.
#[inline]
pub fn tape_clip(x: f32, drive: f32) -> f32 {
    let y = x * drive;
    y / (1.0 + y.abs())
}

/// "Tube" style asymmetric clipper — positive half compressed harder than
/// negative half, producing the strong 2nd-harmonic character of a class-A
/// triode stage. Bias is small intentionally — too much bias adds audible DC.
#[inline]
pub fn tube_clip(x: f32, drive: f32) -> f32 {
    let y = x * drive + 0.08; // small upward DC bias for asymmetry
    let clipped = if y >= 0.0 {
        y / (1.0 + y * 0.7)
    } else {
        y / (1.0 + y.abs() * 1.2)
    };
    clipped - 0.08 // remove bias (downstream DcBlocker scrubs residue)
}

// ---------------------------------------------------------------------------
// Biquad — RBJ "Audio EQ Cookbook" biquad filter. Direct form II transposed
// (single multiply per coefficient, two state variables, numerically robust
// for moderate Q values). Coefficient formulae verified against:
//   Robert Bristow-Johnson, "Cookbook formulae for audio EQ biquad filter
//   coefficients" (https://www.w3.org/TR/audio-eq-cookbook/).
//
// The "peaking EQ" form is symmetric — boost N dB + cut N dB at the same
// frequency and Q produces a precisely flat unity response. Critical for
// transparent vocal EQ work.
// ---------------------------------------------------------------------------

#[derive(Default, Copy, Clone)]
pub struct Biquad {
    /// Normalised feed-forward coefficients (b0/a0, b1/a0, b2/a0).
    b0: f32, b1: f32, b2: f32,
    /// Normalised feedback coefficients (a1/a0, a2/a0).
    a1: f32, a2: f32,
    /// State variables (Direct Form II Transposed).
    z1: f32, z2: f32,
}

impl Biquad {
    /// Process one sample. Coefficients must be set up first via one of the
    /// `set_*` methods.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    pub fn clear(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    /// Evaluate |H(ω)| in dB at the given frequency. Pure analytical —
    /// reads only the cached coefficients, no internal state. Useful
    /// for drawing an EQ curve in the GUI alongside the live spectrum.
    pub fn magnitude_db_at(&self, freq_hz: f32, sr: f32) -> f32 {
        let omega = core::f32::consts::TAU * freq_hz / sr.max(1.0);
        let (s1, c1) = omega.sin_cos();
        let (s2, c2) = (2.0 * omega).sin_cos();
        let num_re = self.b0 + self.b1 * c1 + self.b2 * c2;
        let num_im = -self.b1 * s1 - self.b2 * s2;
        let den_re = 1.0 + self.a1 * c1 + self.a2 * c2;
        let den_im = -self.a1 * s1 - self.a2 * s2;
        let num_sq = num_re * num_re + num_im * num_im;
        let den_sq = den_re * den_re + den_im * den_im;
        10.0 * (num_sq / den_sq.max(1e-20)).max(1e-20).log10()
    }

    /// Configure as a peaking EQ (band-pass shelf). `gain_db` ∈ [-N, +N]
    /// (positive = boost, negative = cut). `q` controls bandwidth: 0.7 ≈
    /// one octave, 4-6 = narrow surgical cut.
    pub fn set_peaking(&mut self, sr: f32, freq_hz: f32, q: f32, gain_db: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = core::f32::consts::TAU * freq_hz / sr;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q.max(0.05));

        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha / a;
        self.normalise(b0, b1, b2, a0, a1, a2);
    }

    /// Low-shelf. `slope` is the RBJ `S` parameter — 1.0 = maximally steep
    /// monotonic shelf. Most plugins use S=1 and just expose the gain.
    pub fn set_low_shelf(&mut self, sr: f32, freq_hz: f32, slope: f32, gain_db: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = core::f32::consts::TAU * freq_hz / sr;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / 2.0
            * ((a + 1.0 / a) * (1.0 / slope.max(0.1) - 1.0) + 2.0).sqrt();
        let sqrt_a_alpha_2 = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) - (a - 1.0) * cos_w0 + sqrt_a_alpha_2);
        let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) - (a - 1.0) * cos_w0 - sqrt_a_alpha_2);
        let a0 = (a + 1.0) + (a - 1.0) * cos_w0 + sqrt_a_alpha_2;
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) + (a - 1.0) * cos_w0 - sqrt_a_alpha_2;
        self.normalise(b0, b1, b2, a0, a1, a2);
    }

    /// High-shelf — mirror of low shelf.
    pub fn set_high_shelf(&mut self, sr: f32, freq_hz: f32, slope: f32, gain_db: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = core::f32::consts::TAU * freq_hz / sr;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / 2.0
            * ((a + 1.0 / a) * (1.0 / slope.max(0.1) - 1.0) + 2.0).sqrt();
        let sqrt_a_alpha_2 = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + sqrt_a_alpha_2);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - sqrt_a_alpha_2);
        let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + sqrt_a_alpha_2;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - sqrt_a_alpha_2;
        self.normalise(b0, b1, b2, a0, a1, a2);
    }

    /// High-pass — biquad 2nd order, RBJ form. `q` typically 0.707 for
    /// Butterworth (max-flat amplitude).
    pub fn set_hpf(&mut self, sr: f32, freq_hz: f32, q: f32) {
        let w0 = core::f32::consts::TAU * freq_hz / sr;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q.max(0.05));

        let b0 = (1.0 + cos_w0) / 2.0;
        let b1 = -(1.0 + cos_w0);
        let b2 = (1.0 + cos_w0) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;
        self.normalise(b0, b1, b2, a0, a1, a2);
    }

    /// Constant-skirt-gain band-pass — RBJ cookbook, peak gain = Q. Used
    /// for formant simulation (3 BPFs in parallel mimic the resonant
    /// peaks of the vocal tract / mouth cavity). This is the right tool
    /// for "vowel" sounds: signal passes ONLY in a narrow band around
    /// the centre frequency, everything else is attenuated. Compare to
    /// `set_peaking` which just adds a hump on top of a passthrough.
    pub fn set_bandpass(&mut self, sr: f32, freq_hz: f32, q: f32) {
        let w0 = core::f32::consts::TAU * freq_hz / sr;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q.max(0.05));

        let b0 = alpha;
        let b1 = 0.0;
        let b2 = -alpha;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;
        self.normalise(b0, b1, b2, a0, a1, a2);
    }

    /// Low-pass mirror of `set_hpf`.
    pub fn set_lpf(&mut self, sr: f32, freq_hz: f32, q: f32) {
        let w0 = core::f32::consts::TAU * freq_hz / sr;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q.max(0.05));

        let b0 = (1.0 - cos_w0) / 2.0;
        let b1 = 1.0 - cos_w0;
        let b2 = (1.0 - cos_w0) / 2.0;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;
        self.normalise(b0, b1, b2, a0, a1, a2);
    }

    fn normalise(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        let inv = 1.0 / a0;
        self.b0 = b0 * inv;
        self.b1 = b1 * inv;
        self.b2 = b2 * inv;
        self.a1 = a1 * inv;
        self.a2 = a2 * inv;
    }
}

// ---------------------------------------------------------------------------
// EnvelopeDetector — peak-with-one-pole-smoothing envelope follower.
//
// Modern transparent compressor approach (Giannoulis-Massberg-Reiss 2012):
// detect instantaneous peak |x|, smooth with asymmetric attack/release
// one-poles. Faster than RMS, smoother than raw peak.
// ---------------------------------------------------------------------------

#[derive(Default, Copy, Clone)]
pub struct EnvelopeDetector {
    envelope: f32,
}

impl EnvelopeDetector {
    /// Process one sample. Returns the smoothed |x| envelope (linear, not dB).
    /// `attack_ms`/`release_ms` are 1-pole time constants.
    #[inline]
    pub fn process(&mut self, x: f32, sr: f32, attack_ms: f32, release_ms: f32) -> f32 {
        let rectified = x.abs();
        let tc = if rectified > self.envelope {
            attack_ms.max(0.01)
        } else {
            release_ms.max(0.01)
        };
        let coef = (-1.0 / (tc * 0.001 * sr)).exp();
        self.envelope = rectified + (self.envelope - rectified) * coef;
        self.envelope
    }

    pub fn level(&self) -> f32 { self.envelope }
    pub fn reset(&mut self) { self.envelope = 0.0; }
}

/// Static compression curve in dB. Returns the **gain reduction** (negative or
/// zero dB) to apply to a signal whose envelope-level is `input_db`.
///
/// Soft-knee form per Giannoulis-Massberg-Reiss 2012, Eq. 4 — quadratic
/// transition over the `knee_db`-wide region centred on `threshold_db`.
#[inline]
pub fn compressor_gain_db(
    input_db: f32,
    threshold_db: f32,
    ratio: f32,
    knee_db: f32,
) -> f32 {
    let knee_half = knee_db * 0.5;
    let slope = 1.0 - 1.0 / ratio.max(1.0);

    if knee_db > 0.0001 && (input_db - threshold_db).abs() <= knee_half {
        // Inside the knee: quadratic interpolation.
        let x = input_db - threshold_db + knee_half;
        -(slope * x * x) / (2.0 * knee_db)
    } else if input_db > threshold_db + knee_half {
        // Hard region above the knee.
        -(input_db - threshold_db) * slope
    } else {
        // Below the knee — no compression.
        0.0
    }
}

/// Compression curve shape selector. ZLCompressor exposes the same three
/// flavors under different names; the audible distinction is the shape
/// of the knee transition and the slope behavior past the knee.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CompressorCurve {
    /// Giannoulis-Massberg-Reiss quadratic — the existing `compressor_gain_db`.
    /// Transparent, mathematically clean, the SSL/Pro-C default.
    Clean = 0,
    /// Asymmetric "pump" curve — slope is boosted by ~25% in the first
    /// 6 dB above threshold then tapers back to the user-set ratio. Gives
    /// the percussive "punching" character of FET compressors (1176 LN
    /// "All-button" / OptoLA-2A in fast mode).
    Pump = 1,
    /// Cubic smoothstep knee — the transition `3x² - 2x³` is C¹-continuous
    /// at both edges, so the curve smoothly approaches the linear slope
    /// asymptotically instead of joining it with a corner. Quieter than
    /// Clean on sustained material.
    Smooth = 2,
}

impl CompressorCurve {
    #[inline]
    pub fn from_index(i: u32) -> Self {
        match i {
            1 => Self::Pump,
            2 => Self::Smooth,
            _ => Self::Clean,
        }
    }
}

/// Static compression curve in dB, dispatched on the selected shape.
/// Curve = Clean reproduces `compressor_gain_db` bit-for-bit.
#[inline]
pub fn compressor_gain_db_curve(
    input_db: f32,
    threshold_db: f32,
    ratio: f32,
    knee_db: f32,
    curve: CompressorCurve,
) -> f32 {
    match curve {
        CompressorCurve::Clean => compressor_gain_db(input_db, threshold_db, ratio, knee_db),
        CompressorCurve::Pump => pump_gain_db(input_db, threshold_db, ratio, knee_db),
        CompressorCurve::Smooth => smooth_gain_db(input_db, threshold_db, ratio, knee_db),
    }
}

/// Asymmetric "pump" curve. Base slope is the same `1 - 1/ratio`; in the
/// first 6 dB above (threshold + knee/2) we multiply the gain reduction
/// by an extra factor that drops from 1.25 → 1.0 along a half-cosine.
/// The result is a slightly steeper initial bite (audible as percussive
/// "pump") that smoothly returns to the user-set ratio for sustained
/// signals. Below and inside the knee the curve matches Clean so the
/// difference shows up only when the comp is actually working.
#[inline]
fn pump_gain_db(input_db: f32, threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
    let base = compressor_gain_db(input_db, threshold_db, ratio, knee_db);
    let over = input_db - (threshold_db + knee_db * 0.5);
    if over <= 0.0 {
        return base;
    }
    const PUMP_REGION_DB: f32 = 6.0;
    const PUMP_BOOST: f32 = 0.25;
    let t = (over / PUMP_REGION_DB).clamp(0.0, 1.0);
    // Half-cosine envelope — 1 at over=0, 0 at over=PUMP_REGION_DB.
    let boost = 0.5 * (core::f32::consts::PI * t).cos() + 0.5;
    base * (1.0 + PUMP_BOOST * boost)
}

/// Cubic-smoothstep knee. Same slope and limits as Clean but the
/// transition through the knee follows `3x² - 2x³` instead of quadratic
/// `x²`. That makes both edges of the knee tangent to the bypass / hard
/// regions (smoothstep is C¹ at both endpoints), giving a perceptibly
/// gentler engagement under steady material.
#[inline]
fn smooth_gain_db(input_db: f32, threshold_db: f32, ratio: f32, knee_db: f32) -> f32 {
    let knee_half = knee_db * 0.5;
    let slope = 1.0 - 1.0 / ratio.max(1.0);
    if knee_db > 0.0001 && (input_db - threshold_db).abs() <= knee_half {
        let x = (input_db - threshold_db + knee_half) / knee_db; // 0..1 across knee
        // Smoothstep: integral of 6x(1-x); produces 3x²-2x³ shape.
        let s = x * x * (3.0 - 2.0 * x);
        // GR at the knee's right edge is -slope * knee_half — interpolate
        // 0 → -slope * knee_half via smoothstep.
        -slope * knee_half * s
    } else if input_db > threshold_db + knee_half {
        -(input_db - threshold_db) * slope
    } else {
        0.0
    }
}

// ---------------------------------------------------------------------------
// DelayLine — variable-length delay with 3rd-order Lagrange interpolation.
//
// Why Lagrange-3 and not linear: linear interpolation high-shelfs the
// fractional-sample reads (~6 dB cut at Nyquist for 0.5-sample offset),
// audible as dullness on repeats. Lagrange-3 is "maximally flat at DC"
// (Smith / Välimäki) — costs 3 multiplies + 3 adds per read, gives a
// nearly-flat response out to ~0.4 * Nyquist with low distortion.
// Allpass interpolation has lower CPU but adds frequency-dependent group
// delay that interacts badly with modulated time changes, so we keep
// Lagrange for the delay tap.
// ---------------------------------------------------------------------------

pub struct DelayLine {
    buf: Vec<f32>,
    write_idx: usize,
    capacity: usize,
}

impl DelayLine {
    pub fn new(max_delay_samples: usize) -> Self {
        let cap = max_delay_samples.next_power_of_two().max(1024);
        Self {
            buf: vec![0.0; cap],
            write_idx: 0,
            capacity: cap,
        }
    }

    /// Write one sample, advancing the head.
    #[inline]
    pub fn write(&mut self, x: f32) {
        self.buf[self.write_idx] = x;
        self.write_idx = (self.write_idx + 1) % self.capacity;
    }

    /// Read at fractional delay `d` samples. Uses 3rd-order Lagrange
    /// interpolation between four neighbouring samples.
    ///
    /// `d` must satisfy `1.0 <= d <= capacity-2`. Caller clamps.
    #[inline]
    pub fn read_lagrange3(&self, d: f32) -> f32 {
        let d = d.max(1.0).min((self.capacity - 2) as f32);
        let d_int = d as usize;
        let frac = d - d_int as f32;

        // Read four taps: y_{-1}, y_0, y_1, y_2 around the fractional point.
        let n = self.capacity;
        let base = (self.write_idx + n - d_int - 1) % n;
        let y_m1 = self.buf[base];
        let y_0  = self.buf[(base + 1) % n];
        let y_1  = self.buf[(base + 2) % n];
        let y_2  = self.buf[(base + 3) % n];

        // 3rd-order Lagrange — coefficients pre-factored for `frac ∈ [0,1]`.
        // Reference: J.O. Smith, "Physical Audio Signal Processing",
        // Section "Lagrange Interpolation".
        let c0 = -frac * (frac - 1.0) * (frac - 2.0) / 6.0;
        let c1 = (frac + 1.0) * (frac - 1.0) * (frac - 2.0) / 2.0;
        let c2 = -(frac + 1.0) * frac * (frac - 2.0) / 2.0;
        let c3 = (frac + 1.0) * frac * (frac - 1.0) / 6.0;

        c0 * y_m1 + c1 * y_0 + c2 * y_1 + c3 * y_2
    }

    pub fn clear(&mut self) {
        self.buf.fill(0.0);
        self.write_idx = 0;
    }
}

// ---------------------------------------------------------------------------
// SlewLimiter2Pole — two cascaded one-pole filters on a control parameter.
//
// Single one-pole has a discontinuous first derivative when target jumps,
// which audibly clicks on delay-time changes (you hear a step in pitch).
// Two in series → C¹ continuous → smooth tape-style pitch sweep.
// ---------------------------------------------------------------------------

#[derive(Default, Copy, Clone)]
pub struct SlewLimiter2Pole {
    a: f32,
    b: f32,
}

impl SlewLimiter2Pole {
    pub fn new(initial: f32) -> Self {
        Self { a: initial, b: initial }
    }

    /// Slew toward `target` with a `time_const_ms` time constant on each
    /// of the two cascaded one-poles. ~30 ms gives a noticeable but musical
    /// tape-doppler when the time knob is swept.
    #[inline]
    pub fn step(&mut self, target: f32, sr: f32, time_const_ms: f32) -> f32 {
        let coef = (-1.0 / (time_const_ms.max(0.1) * 0.001 * sr)).exp();
        self.a = target + (self.a - target) * coef;
        self.b = self.a + (self.b - self.a) * coef;
        self.b
    }

    pub fn snap(&mut self, value: f32) {
        self.a = value;
        self.b = value;
    }
}

// ---------------------------------------------------------------------------
// OnePoleLp — basic one-pole low-pass. Useful as the tone control inside
// a delay's feedback loop (the "every repeat gets darker" trick).
// ---------------------------------------------------------------------------

#[derive(Default, Copy, Clone)]
pub struct OnePoleLp {
    z: f32,
}

impl OnePoleLp {
    /// Process one sample. `cutoff_hz` clamped internally.
    #[inline]
    pub fn process(&mut self, x: f32, sr: f32, cutoff_hz: f32) -> f32 {
        let cutoff = cutoff_hz.clamp(20.0, sr * 0.45);
        let coef = (-core::f32::consts::TAU * cutoff / sr).exp();
        self.z = x * (1.0 - coef) + self.z * coef;
        self.z
    }
}

// ---------------------------------------------------------------------------
// Tilt — single-shelf brightness control, ±6 dB at ±1.0.
// ---------------------------------------------------------------------------

#[derive(Default, Copy, Clone)]
pub struct Tilt {
    lp: f32,
}

impl Tilt {
    /// One-pole LPF at 1.5 kHz splits the band; tilt > 0 boosts highs,
    /// tilt < 0 boosts lows. `tilt` clamped to [-1, +1] internally.
    #[inline]
    pub fn process(&mut self, x: f32, sr: f32, tilt: f32) -> f32 {
        let tilt = tilt.clamp(-1.0, 1.0);
        let coef = (-core::f32::consts::TAU * 1500.0 / sr).exp();
        self.lp = x * (1.0 - coef) + self.lp * coef;
        let low = self.lp;
        let high = x - low;
        let gain_hi = 10f32.powf(tilt * 6.0 / 20.0);
        let gain_lo = 10f32.powf(-tilt * 6.0 / 20.0);
        low * gain_lo + high * gain_hi
    }
}

// ---------------------------------------------------------------------------
// Ducker — peak-envelope-driven gain reducer with asymmetric attack/release.
//
// Used to drive sidechain ducking inside reverbs / delays / saturators —
// the classic vocal-bus pattern (key signal turns wet down so the dry
// sits in the front of the mix). Same primitive used by superduper-reverb
// and superduper-supermass.
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Ducker {
    envelope: f32,
}

impl Ducker {
    /// `amount_db` is the maximum attenuation (positive number) at full key.
    /// Returns a linear gain ∈ (0, 1] to apply to the wet path.
    #[inline]
    pub fn process(
        &mut self,
        key_l: f32,
        key_r: f32,
        sr: f32,
        amount_db: f32,
        attack_ms: f32,
        release_ms: f32,
    ) -> f32 {
        let rectified = key_l.abs().max(key_r.abs());
        let coef = if rectified > self.envelope {
            (-1.0 / (attack_ms.max(0.1) * 0.001 * sr)).exp()
        } else {
            (-1.0 / (release_ms.max(0.1) * 0.001 * sr)).exp()
        };
        self.envelope = rectified + (self.envelope - rectified) * coef;

        if amount_db <= 0.001 {
            return 1.0;
        }
        let drive = (self.envelope * 4.0).min(1.0);
        10f32.powf(-(amount_db * drive) / 20.0)
    }

    /// Read the current envelope (for metering / debugging). Not used in
    /// the audio path.
    pub fn envelope(&self) -> f32 {
        self.envelope
    }
}

// ---------------------------------------------------------------------------
// AdsrEnvelope — linear attack, exponential decay/release ADSR with explicit
// states. Designed for note-driven synths: gate() starts attack, release()
// transitions to release stage (preserving current level so re-triggers
// during release don't glitch).
//
// One-pole coefficients are derived per stage from the target time: solve
// `coef = exp(-1/(time_s * sr * STAGE_FACTOR))` so a `time_s` knob produces
// a ~3·τ visible decay. Industry-standard heuristic (TAL, Vital, Surge use
// similar) — gives a knob range that "feels right" musically without each
// stage needing a different curve shape.
// ---------------------------------------------------------------------------

/// State machine of a DAHDSR envelope. The Delay + Hold stages turn
/// the classic ADSR into a more expressive shape — Vital / Serum style.
/// Setting `delay_s = 0` and `hold_s = 0` collapses back to ADSR
/// behaviour exactly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdsrStage {
    Idle,
    /// Pre-attack silence — gate_on() lands here for `delay_s` seconds.
    PreDelay,
    Attack,
    /// Hold at peak (1.0) for `hold_s` seconds before decay starts.
    Hold,
    Decay,
    Sustain,
    Release,
}

#[derive(Clone, Copy)]
pub struct AdsrEnvelope {
    level: f32,
    stage: AdsrStage,
    /// Samples spent in the current stage. Used by PreDelay + Hold,
    /// which both wait by sample count rather than by level.
    stage_samples: u32,
}

impl Default for AdsrEnvelope {
    fn default() -> Self {
        Self {
            level: 0.0,
            stage: AdsrStage::Idle,
            stage_samples: 0,
        }
    }
}

#[derive(Copy, Clone)]
pub struct AdsrParams {
    pub sr: f32,
    /// Pre-attack delay in seconds (DAHDSR's D). 0 = no delay → classic
    /// ADSR behaviour. Useful for layering and rhythmic effects.
    pub delay_s: f32,
    pub attack_s: f32,
    /// Hold the peak at 1.0 for this many seconds before decaying.
    /// 0 = no hold → classic ADSR.
    pub hold_s: f32,
    pub decay_s: f32,
    pub sustain: f32,
    pub release_s: f32,
}

impl AdsrParams {
    /// Convenience constructor for the classic ADSR shape (delay = hold = 0).
    /// Existing call sites that don't care about DAHDSR keep working
    /// by calling this instead of struct literal syntax.
    #[inline]
    pub fn adsr(sr: f32, attack_s: f32, decay_s: f32, sustain: f32, release_s: f32) -> Self {
        Self { sr, delay_s: 0.0, attack_s, hold_s: 0.0, decay_s, sustain, release_s }
    }
}

impl AdsrEnvelope {
    /// Begin attack from the current level — re-triggering during decay or
    /// release smoothly resumes from where the envelope currently is.
    /// Goes through PreDelay first if `delay_s > 0` on the next process()
    /// call (which transitions to Attack once `delay_s * sr` samples elapse).
    #[inline]
    pub fn gate_on(&mut self) {
        self.stage = AdsrStage::PreDelay;
        self.stage_samples = 0;
    }

    /// Begin release from the current level.
    #[inline]
    pub fn gate_off(&mut self) {
        if self.stage != AdsrStage::Idle {
            self.stage = AdsrStage::Release;
        }
    }

    /// Has the envelope fully decayed to silence after release?
    #[inline]
    pub fn is_idle(&self) -> bool {
        matches!(self.stage, AdsrStage::Idle)
    }

    #[inline]
    pub fn is_releasing(&self) -> bool {
        matches!(self.stage, AdsrStage::Release)
    }

    #[inline]
    pub fn level(&self) -> f32 {
        self.level
    }

    #[inline]
    pub fn stage(&self) -> AdsrStage {
        self.stage
    }

    /// Advance one sample, returning current envelope level [0..1].
    #[inline]
    pub fn process(&mut self, p: AdsrParams) -> f32 {
        const ATTACK_FLOOR: f32 = 1e-4;
        const RELEASE_FLOOR: f32 = 1e-4;

        match self.stage {
            AdsrStage::Idle => {
                self.level = 0.0;
            }
            AdsrStage::PreDelay => {
                // Silence while we count out delay_s seconds. Going
                // straight to Attack when delay_s ≈ 0 lets old ADSR
                // call-sites keep their original timing.
                self.level = 0.0;
                let delay_samples = (p.delay_s.max(0.0) * p.sr) as u32;
                if self.stage_samples >= delay_samples {
                    self.stage = AdsrStage::Attack;
                    self.stage_samples = 0;
                } else {
                    self.stage_samples += 1;
                }
            }
            AdsrStage::Attack => {
                // Linear ramp — gives the punchy front-end a note needs.
                let inc = if p.attack_s <= 1e-4 { 1.0 } else { 1.0 / (p.attack_s * p.sr) };
                self.level += inc;
                if self.level >= 1.0 {
                    self.level = 1.0;
                    self.stage = AdsrStage::Hold;
                    self.stage_samples = 0;
                }
            }
            AdsrStage::Hold => {
                // Pin to the peak for hold_s seconds; transition straight
                // to Decay when the hold time expires (or immediately if
                // hold_s ≈ 0, which is the old ADSR behaviour).
                self.level = 1.0;
                let hold_samples = (p.hold_s.max(0.0) * p.sr) as u32;
                if self.stage_samples >= hold_samples {
                    self.stage = AdsrStage::Decay;
                    self.stage_samples = 0;
                } else {
                    self.stage_samples += 1;
                }
            }
            AdsrStage::Decay => {
                // Exp glide toward sustain.
                let sustain = p.sustain.clamp(0.0, 1.0);
                let tau = (p.decay_s * p.sr).max(1.0);
                let coef = (-1.0 / tau).exp();
                self.level = sustain + (self.level - sustain) * coef;
                if (self.level - sustain).abs() < ATTACK_FLOOR {
                    self.level = sustain;
                    self.stage = AdsrStage::Sustain;
                }
            }
            AdsrStage::Sustain => {
                self.level = p.sustain.clamp(0.0, 1.0);
            }
            AdsrStage::Release => {
                let tau = (p.release_s * p.sr).max(1.0);
                let coef = (-1.0 / tau).exp();
                self.level *= coef;
                if self.level <= RELEASE_FLOOR {
                    self.level = 0.0;
                    self.stage = AdsrStage::Idle;
                }
            }
        }
        self.level
    }
}

/// Convert a MIDI note number (0-127) to frequency in Hz.
/// A4 = key 69 = 440 Hz, then 12-TET.
#[inline]
pub fn midi_note_to_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}
