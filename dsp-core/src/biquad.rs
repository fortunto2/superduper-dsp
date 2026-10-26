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

/// RBJ's ω₀ with the frequency held below Nyquist: at a 1234 Hz sample rate
/// a 2 kHz formant or a 6 kHz de-esser band put the pole outside the unit
/// circle and the output went NaN within 200 samples (clap-validator
/// process-varying-sample-rates, 4 plugins, 2026-10-21).
#[inline]
fn rbj_w0(sr: f32, freq_hz: f32) -> f32 {
    let f = if freq_hz.is_finite() { freq_hz.clamp(1.0, sr * 0.49) } else { sr * 0.1 };
    core::f32::consts::TAU * f / sr
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
        let w0 = rbj_w0(sr, freq_hz);
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
        let w0 = rbj_w0(sr, freq_hz);
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / 2.0
            * ((a + 1.0 / a) * (1.0 / slope.clamp(0.1, 1.0) - 1.0) + 2.0).sqrt(); // S > 1 → negative radicand → NaN
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
        let w0 = rbj_w0(sr, freq_hz);
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / 2.0
            * ((a + 1.0 / a) * (1.0 / slope.clamp(0.1, 1.0) - 1.0) + 2.0).sqrt(); // S > 1 → negative radicand → NaN
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
        let w0 = rbj_w0(sr, freq_hz);
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
        let w0 = rbj_w0(sr, freq_hz);
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
        let w0 = rbj_w0(sr, freq_hz);
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

    /// Configure as a notch filter (band-stop). `freq_hz` = centre,
    /// `q` = sharpness (10..50 for surgical hum removal; 30 ≈ 2 Hz
    /// wide at 50 Hz which leaves the rest of the band untouched).
    pub fn set_notch(&mut self, sr: f32, freq_hz: f32, q: f32) {
        let w0 = rbj_w0(sr, freq_hz);
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / (2.0 * q.max(0.5));

        let b0 = 1.0;
        let b1 = -2.0 * cos_w0;
        let b2 = 1.0;
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
