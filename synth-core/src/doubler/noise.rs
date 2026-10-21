//! Slow, smooth, band-limited random wander — the "hands" of a double-tracked
//! singer. A singer re-singing a line is never a fixed offset away: they drift
//! early and late, sharp and flat, at well under a hertz. White noise through a
//! filter would wander too, but its excursion is unbounded in practice and its
//! spectrum has a tail; value noise with Catmull-Rom interpolation stays inside
//! [-1, 1], has a continuous first derivative (no kinks you can hear as pitch
//! steps) and almost all of its energy below the point rate.

use crate::dsp_blocks::Xorshift;

/// Mix a seed, a voice index and a stream id into one xorshift seed (splitmix32
/// finaliser). Different voices of one seed, and the same voice of different
/// seeds, must never share a sequence.
pub fn mix_seed(seed: u32, voice: u32, stream: u32) -> u32 {
    let mut z = seed
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(voice.wrapping_mul(0x85EB_CA6B))
        .wrapping_add(stream.wrapping_mul(0xC2B2_AE35))
        .wrapping_add(0x6A09_E667);
    z = (z ^ (z >> 16)).wrapping_mul(0x7FEB_352D);
    z = (z ^ (z >> 15)).wrapping_mul(0x846C_A68B);
    z ^ (z >> 16)
}

/// Value noise: random points at `rate_hz × 2`, Catmull-Rom between them.
#[derive(Clone, Copy)]
pub struct SmoothNoise {
    rng: Xorshift,
    p: [f32; 4],
    phase: f32,
    inc: f32,
}

impl SmoothNoise {
    /// `rate_hz` is the wander rate: points arrive twice per cycle, so the
    /// output's energy sits at and below `rate_hz`.
    pub fn new(seed: u32, rate_hz: f32, sr: f32) -> Self {
        let mut rng = Xorshift::new(seed);
        let p = [
            rng.next_bipolar(),
            rng.next_bipolar(),
            rng.next_bipolar(),
            rng.next_bipolar(),
        ];
        // Start part-way between points so voices do not all begin on a point.
        let phase = rng.next_f32();
        Self { rng, p, phase, inc: 2.0 * rate_hz.max(0.01) / sr.max(1.0) }
    }

    /// Current value without advancing, in [-1, 1].
    #[inline]
    pub fn value(&self) -> f32 {
        let t = self.phase;
        let [p0, p1, p2, p3] = self.p;
        // Catmull-Rom between p1 and p2.
        let a = -0.5 * p0 + 1.5 * p1 - 1.5 * p2 + 0.5 * p3;
        let b = p0 - 2.5 * p1 + 2.0 * p2 - 0.5 * p3;
        let c = -0.5 * p0 + 0.5 * p2;
        (((a * t + b) * t + c) * t + p1).clamp(-1.0, 1.0)
    }

    /// Advance `n` samples and return the new value.
    #[inline]
    pub fn advance(&mut self, n: u32) -> f32 {
        self.phase += self.inc * n as f32;
        while self.phase >= 1.0 {
            self.phase -= 1.0;
            self.p = [self.p[1], self.p[2], self.p[3], self.rng.next_bipolar()];
        }
        self.value()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stays_in_range_and_moves_slowly() {
        let sr = 48_000.0;
        let mut n = SmoothNoise::new(7, 0.5, sr);
        let (mut lo, mut hi) = (1.0f32, -1.0f32);
        let mut prev = n.value();
        let mut max_step = 0.0f32;
        for _ in 0..(20.0 * sr) as usize {
            let v = n.advance(1);
            lo = lo.min(v);
            hi = hi.max(v);
            max_step = max_step.max((v - prev).abs());
            prev = v;
        }
        assert!(lo >= -1.0 && hi <= 1.0);
        assert!(hi - lo > 1.0, "20 s of wander should cover most of the range: {lo}..{hi}");
        // A 0.5 Hz wander moves at most a few units per second, so per sample
        // the step is tiny — no audible kinks.
        assert!(max_step < 1e-3, "step {max_step}");
    }

    #[test]
    fn seeds_differ() {
        let a = mix_seed(1, 0, 0);
        assert_ne!(a, mix_seed(1, 1, 0));
        assert_ne!(a, mix_seed(2, 0, 0));
        assert_ne!(a, mix_seed(1, 0, 1));
    }
}
