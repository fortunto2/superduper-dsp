//! The two RBJ biquads K-weighting needs, byte-for-byte the math of
//! `superduper_synth_core::dsp_blocks::Biquad` (same ω₀ clamp, same
//! normalisation), so moving the meter here changed no reading.

#[derive(Default, Copy, Clone)]
pub struct Biquad {
    b0: f32, b1: f32, b2: f32,
    a1: f32, a2: f32,
    z1: f32, z2: f32,
}

#[inline]
fn rbj_w0(sr: f32, freq_hz: f32) -> f32 {
    let f = if freq_hz.is_finite() { freq_hz.clamp(1.0, sr * 0.49) } else { sr * 0.1 };
    core::f32::consts::TAU * f / sr
}

impl Biquad {
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

    pub fn set_high_shelf(&mut self, sr: f32, freq_hz: f32, slope: f32, gain_db: f32) {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = rbj_w0(sr, freq_hz);
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        let alpha = sin_w0 / 2.0
            * ((a + 1.0 / a) * (1.0 / slope.clamp(0.1, 1.0) - 1.0) + 2.0).sqrt();
        let sqrt_a_alpha_2 = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + sqrt_a_alpha_2);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - sqrt_a_alpha_2);
        let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + sqrt_a_alpha_2;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - sqrt_a_alpha_2;
        self.normalise(b0, b1, b2, a0, a1, a2);
    }

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

    fn normalise(&mut self, b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) {
        let inv = 1.0 / a0;
        self.b0 = b0 * inv;
        self.b1 = b1 * inv;
        self.b2 = b2 * inv;
        self.a1 = a1 * inv;
        self.a2 = a2 * inv;
    }
}
