//! 3-band parallel bandpass formant filter.
//!
//! Models a vocal tract by stacking three resonant bandpass peaks (F1, F2,
//! F3). The same DSP shape as the formant block in KubizBeat's KubyzVoice
//! (`BandPassButterworthFilter` × 3 → mixer). Useful for vowel-like timbres,
//! "talking" basses, and jaw-harp / khomus character.
//!
//! Each band is an RBJ biquad bandpass (constant 0 dB peak) tuned to its
//! centre frequency with bandwidth derived from Q. The three outputs are
//! summed at equal weight, then blended with the dry signal via Mix.
//!
//! Vowel presets and the Bashkir/khomus preset come straight from the
//! KubizBeat sample data — see `VOWELS` and `BASHKIR` constants below.

use crate::dsp_blocks::Biquad;

/// One stereo-paired 3-band formant.
#[derive(Default, Clone, Copy)]
pub struct Formant {
    bp_l: [Biquad; 3],
    bp_r: [Biquad; 3],
    cached: CachedTuning,
}

#[derive(Default, Clone, Copy, PartialEq)]
struct CachedTuning {
    f: [f32; 3],
    bw: [f32; 3],
    sr: f32,
}

impl Formant {
    /// Recompute biquad coefficients only when tuning changed (cheap cache).
    fn ensure_tuned(&mut self, sr: f32, f: [f32; 3], bw: [f32; 3]) {
        let next = CachedTuning { f, bw, sr };
        if self.cached == next {
            return;
        }
        for i in 0..3 {
            // Q = centre / bandwidth (RBJ definition). Clamp to keep biquad
            // sane — Q above ~12 starts ringing forever at sustained input.
            let q = (f[i] / bw[i].max(20.0)).clamp(0.5, 12.0);
            // True band-pass (not peaking!) — this is what gives a vowel
            // its character: signal passes ONLY in a narrow band around
            // the centre frequency. KubizBeat's KubyzVoice uses the same
            // shape (`BandPassButterworthFilter` × 3 → mixer).
            self.bp_l[i].set_bandpass(sr, f[i], q);
            self.bp_r[i].set_bandpass(sr, f[i], q);
        }
        self.cached = next;
    }

    /// Process one stereo pair through the 3-band formant. `mix` ∈ [0,1] —
    /// 0 = dry, 1 = full formant; linearly crossfaded.
    #[inline]
    pub fn process(
        &mut self,
        l: f32,
        r: f32,
        sr: f32,
        f: [f32; 3],
        bw: [f32; 3],
        gains: [f32; 3],
        mix: f32,
    ) -> (f32, f32) {
        if mix <= 0.0 {
            return (l, r);
        }
        self.ensure_tuned(sr, f, bw);
        let mut wet_l = 0.0_f32;
        let mut wet_r = 0.0_f32;
        for i in 0..3 {
            wet_l += self.bp_l[i].process(l) * gains[i];
            wet_r += self.bp_r[i].process(r) * gains[i];
        }
        // BPF outputs peak around Q, so for our F/BW ratios (typically
        // Q ≈ 3–5) the sum's peaks land around -10 dBFS relative to a
        // unity sine at the formant centre. Add a small make-up gain so
        // Mix=1 lands at roughly the dry signal's loudness; tanh-soft
        // catches the rare moments the three bands coincide.
        let mix = mix.clamp(0.0, 1.0);
        let makeup = 1.8;
        let out_l = l * (1.0 - mix) + (wet_l * makeup).tanh() * mix;
        let out_r = r * (1.0 - mix) + (wet_r * makeup).tanh() * mix;
        (out_l, out_r)
    }
}

// ---------------------------------------------------------------------------
// Vowel & instrument presets — F1/F2/F3 + bandwidths.
// Source: Peterson & Barney (1952) for vowels, KubizBeat repo for Bashkir.
// ---------------------------------------------------------------------------

/// A formant set: three centre frequencies + bandwidths (Hz).
#[derive(Clone, Copy)]
pub struct FormantPreset {
    pub name: &'static str,
    pub f: [f32; 3],
    pub bw: [f32; 3],
    /// Per-band gain (1.0 = neutral). Used to emphasise F1 over F3 etc.
    pub gain: [f32; 3],
}

pub const FORMANT_PRESETS: &[FormantPreset] = &[
    FormantPreset {
        name: "Off",
        f: [700.0, 1200.0, 2600.0],
        bw: [200.0, 300.0, 400.0],
        gain: [1.0, 1.0, 1.0],
    },
    // ---- Vowels (male average from Peterson-Barney 1952) ----
    FormantPreset {
        name: "Vowel A (/ɑ/)",
        f: [730.0, 1090.0, 2440.0],
        bw: [130.0, 180.0, 260.0],
        gain: [1.0, 0.8, 0.6],
    },
    FormantPreset {
        name: "Vowel E (/ɛ/)",
        f: [530.0, 1840.0, 2480.0],
        bw: [120.0, 200.0, 260.0],
        gain: [1.0, 0.85, 0.6],
    },
    FormantPreset {
        name: "Vowel I (/i/)",
        f: [270.0, 2290.0, 3010.0],
        bw: [100.0, 200.0, 280.0],
        gain: [1.0, 0.9, 0.5],
    },
    FormantPreset {
        name: "Vowel O (/ɔ/)",
        f: [570.0, 840.0, 2410.0],
        bw: [130.0, 170.0, 260.0],
        gain: [1.0, 0.75, 0.5],
    },
    FormantPreset {
        name: "Vowel U (/u/)",
        f: [300.0, 870.0, 2240.0],
        bw: [120.0, 180.0, 260.0],
        gain: [1.0, 0.6, 0.4],
    },
    // ---- KubizBeat references (Bashkir / sample target khomus) ----
    FormantPreset {
        name: "Bashkir Kubyz",
        f: [705.0, 1301.0, 2165.0],
        bw: [200.0, 300.0, 400.0],
        gain: [1.0, 0.9, 0.75],
    },
    FormantPreset {
        name: "Khomus Sample",
        f: [702.0, 1365.0, 2115.0],
        bw: [200.0, 300.0, 400.0],
        gain: [1.0, 0.9, 0.7],
    },
];
