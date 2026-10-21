//! Morph's targets as plugin state: the Kubyz model (factory tables + the
//! mailbox-editable copy), the captured timbre, and the JSON they persist as.
//!
//! The atomics here are the hand-off between threads: the GUI / mailbox /
//! state loader write them on the main thread, the audio thread reads them
//! when their generation counter moves, and writes the captured profile back
//! when Capture fires. A torn read can only mix two consecutive versions of
//! one table for one block, which is inaudible.

use atomic_float::AtomicF32;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use superduper_synth_core::kubyz::formant_bank::{FormantBands, MAX_FORMANTS};
use superduper_synth_core::kubyz::N_HARMONICS;
use superduper_synth_core::timbre::{ModelTarget, TimbreProfile, GRID, MAX_HARM};

// ---------------------------------------------------------------------------
// Kubyz factory tables.
// AI-NOTE: a copy of effects/superduper-kubyz/src/presets.rs (that crate is a
// cdylib; linking it would export two clap_entry symbols). tests/targets.rs
// pins this copy against the original through a dev-dependency.
// ---------------------------------------------------------------------------

pub const KUBYZ_COUNT: usize = 5;
pub const KUBYZ_NAMES: [&str; KUBYZ_COUNT] =
    ["Init (sine)", "Bashkir Kubyz", "Khomus Sample", "Real D2 (sample-fit)", "Aulos (Odyssey)"];

const H_DB: [[f32; N_HARMONICS]; KUBYZ_COUNT] = [
    // Init: H1 only (handled below, the dB row is unused).
    [0.0; N_HARMONICS],
    [0.0, 6.6, 19.0, 24.1, 17.0, 38.6, 9.4, 16.7, 15.2, 17.9, 19.9, 9.8, 14.3, -0.5, 7.6, 3.3],
    [0.0, 2.6, 17.6, 24.8, 25.4, 31.4, 19.6, 20.0, 7.9, 19.7, 16.4, 16.8, 16.4, 8.0, 16.3, 3.1],
    [
        0.0, -33.83, -9.50, -14.71, -16.52, -26.03, -11.07, -20.27, -16.22, -9.19, 27.25, -0.99,
        34.42, 20.46, 45.10, 13.55,
    ],
    [0.0, 4.0, 6.5, 8.0, 7.0, 5.0, 1.0, -2.0, -4.0, -6.0, -8.0, -10.0, -12.0, -14.0, -16.0, -18.0],
];
/// (f, bw, gain) per preset — Off, Bashkir, Khomus, Real D2, Aulos.
const FORMANTS: [([f32; 3], [f32; 3], [f32; 3]); KUBYZ_COUNT] = [
    ([700.0, 1200.0, 2600.0], [200.0, 300.0, 400.0], [1.0, 1.0, 1.0]),
    ([705.0, 1301.0, 2165.0], [200.0, 300.0, 400.0], [1.0, 0.9, 0.75]),
    ([702.0, 1365.0, 2115.0], [200.0, 300.0, 400.0], [1.0, 0.9, 0.7]),
    ([809.0, 1091.0, 1242.0], [50.0, 50.0, 50.0], [1.0, 1.0, 1.0]),
    ([320.0, 560.0, 780.0], [180.0, 220.0, 260.0], [1.0, 0.85, 0.6]),
];
/// The preset's `default_vox_mix` — how much of the mouth formant you hear.
const VOX_MIX: [f32; KUBYZ_COUNT] = [0.0, 1.0, 1.0, 0.5, 1.0];

/// Linear harmonic amplitudes of factory preset `i`, normalised to the
/// loudest (exactly as Kubyz's `db_to_lin_array`).
pub fn kubyz_harmonics(i: usize) -> [f32; N_HARMONICS] {
    let i = i.min(KUBYZ_COUNT - 1);
    if i == 0 {
        let mut h = [0.0; N_HARMONICS];
        h[0] = 1.0;
        return h;
    }
    let mut out = H_DB[i].map(|db| 10f32.powf(db / 20.0));
    let max = out.iter().copied().fold(0.0f32, f32::max).max(1e-6);
    out.iter_mut().for_each(|x| *x /= max);
    out
}

pub fn kubyz_model(i: usize) -> ModelTarget {
    let i = i.min(KUBYZ_COUNT - 1);
    let (f, bw, g) = FORMANTS[i];
    ModelTarget { harmonics: kubyz_harmonics(i), bands: FormantBands::three(f, bw, g), vox_mix: VOX_MIX[i] }
}

// ---------------------------------------------------------------------------
// Thread hand-off stores
// ---------------------------------------------------------------------------

pub struct ModelStore {
    pub harmonics: [AtomicF32; N_HARMONICS],
    pub f: [AtomicF32; MAX_FORMANTS],
    pub bw: [AtomicF32; MAX_FORMANTS],
    pub gain: [AtomicF32; MAX_FORMANTS],
    pub count: AtomicU32,
    pub vox_mix: AtomicF32,
    pub gen: AtomicU32,
    /// Which `Target` param value the model currently reflects. The audio
    /// thread reloads the factory table only when the param moves away from
    /// it, so a model set by the mailbox or restored from a project survives.
    pub from_target: AtomicU32,
}

impl ModelStore {
    pub fn new(m: &ModelTarget, target: u32) -> Self {
        let s = Self {
            harmonics: std::array::from_fn(|_| AtomicF32::new(0.0)),
            f: std::array::from_fn(|_| AtomicF32::new(0.0)),
            bw: std::array::from_fn(|_| AtomicF32::new(0.0)),
            gain: std::array::from_fn(|_| AtomicF32::new(0.0)),
            count: AtomicU32::new(3),
            vox_mix: AtomicF32::new(1.0),
            gen: AtomicU32::new(0),
            from_target: AtomicU32::new(target),
        };
        s.store(m);
        s
    }
    pub fn load(&self) -> ModelTarget {
        let ld = |a: &AtomicF32| a.load(Ordering::Relaxed);
        ModelTarget {
            harmonics: std::array::from_fn(|i| ld(&self.harmonics[i])),
            bands: FormantBands {
                f: std::array::from_fn(|i| ld(&self.f[i])),
                bw: std::array::from_fn(|i| ld(&self.bw[i])),
                gain: std::array::from_fn(|i| ld(&self.gain[i])),
                count: (self.count.load(Ordering::Relaxed) as usize).clamp(1, MAX_FORMANTS),
            },
            vox_mix: ld(&self.vox_mix),
        }
    }
    pub fn store(&self, m: &ModelTarget) {
        for (a, v) in self.harmonics.iter().zip(m.harmonics) {
            a.store(v, Ordering::Relaxed);
        }
        for i in 0..MAX_FORMANTS {
            self.f[i].store(m.bands.f[i], Ordering::Relaxed);
            self.bw[i].store(m.bands.bw[i], Ordering::Relaxed);
            self.gain[i].store(m.bands.gain[i], Ordering::Relaxed);
        }
        self.count.store(m.bands.count.clamp(1, MAX_FORMANTS) as u32, Ordering::Relaxed);
        self.vox_mix.store(m.vox_mix, Ordering::Relaxed);
        self.gen.fetch_add(1, Ordering::Release);
    }
}

pub struct ProfileStore {
    pub f0: AtomicF32,
    pub harm_valid: AtomicBool,
    pub harm_db: [AtomicF32; MAX_HARM],
    pub env_valid: AtomicBool,
    pub env_db: [AtomicF32; GRID],
    pub noise: AtomicF32,
    pub gen: AtomicU32,
}

impl ProfileStore {
    pub fn new() -> Self {
        let s = Self {
            f0: AtomicF32::new(0.0),
            harm_valid: AtomicBool::new(false),
            harm_db: std::array::from_fn(|_| AtomicF32::new(0.0)),
            env_valid: AtomicBool::new(false),
            env_db: std::array::from_fn(|_| AtomicF32::new(0.0)),
            noise: AtomicF32::new(1.0),
            gen: AtomicU32::new(0),
        };
        s.store(&TimbreProfile::default());
        s
    }
    pub fn load(&self) -> TimbreProfile {
        let ld = |a: &AtomicF32| a.load(Ordering::Relaxed);
        TimbreProfile {
            f0: ld(&self.f0),
            harm_valid: self.harm_valid.load(Ordering::Relaxed),
            harm_db: std::array::from_fn(|i| ld(&self.harm_db[i])),
            env_valid: self.env_valid.load(Ordering::Relaxed),
            env_db: std::array::from_fn(|i| ld(&self.env_db[i])),
            noise: ld(&self.noise),
        }
    }
    pub fn store(&self, p: &TimbreProfile) {
        self.f0.store(p.f0, Ordering::Relaxed);
        self.harm_valid.store(p.harm_valid, Ordering::Relaxed);
        for (a, v) in self.harm_db.iter().zip(p.harm_db) {
            a.store(v, Ordering::Relaxed);
        }
        self.env_valid.store(p.env_valid, Ordering::Relaxed);
        for (a, v) in self.env_db.iter().zip(p.env_db) {
            a.store(v, Ordering::Relaxed);
        }
        self.noise.store(p.noise, Ordering::Relaxed);
        self.gen.fetch_add(1, Ordering::Release);
    }
}

impl Default for ProfileStore {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// JSON forms (state + mailbox)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Band {
    pub f: f32,
    #[serde(default = "default_bw")]
    pub bw: f32,
    #[serde(default = "default_gain")]
    pub gain: f32,
}
fn default_bw() -> f32 {
    200.0
}
fn default_gain() -> f32 {
    1.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelJson {
    pub harmonics: Vec<f32>,
    pub formants: Vec<Band>,
    pub vox_mix: f32,
}

impl ModelJson {
    pub fn from_model(m: &ModelTarget) -> Self {
        let n = m.bands.live();
        Self {
            harmonics: m.harmonics.to_vec(),
            formants: (0..n).map(|i| Band { f: m.bands.f[i], bw: m.bands.bw[i], gain: m.bands.gain[i] }).collect(),
            vox_mix: m.vox_mix,
        }
    }
    /// Validate + clamp into a model. Unlisted harmonics are 0 (a short list
    /// means "only these", as in Kubyz's doc).
    pub fn to_model(&self) -> Result<ModelTarget, String> {
        if self.harmonics.len() > N_HARMONICS {
            return Err(format!("harmonics: at most {N_HARMONICS}"));
        }
        if !(1..=MAX_FORMANTS).contains(&self.formants.len()) {
            return Err(format!("formants: 1..={MAX_FORMANTS} bands"));
        }
        let ok = |x: f32| x.is_finite();
        if self.harmonics.iter().any(|&h| !ok(h))
            || self.formants.iter().any(|b| !ok(b.f) || !ok(b.bw) || b.bw <= 0.0 || !ok(b.gain))
            || !ok(self.vox_mix)
        {
            return Err("values must be finite, bw > 0".into());
        }
        let mut bands = FormantBands::three([700.0, 1200.0, 2600.0], [200.0; 3], [1.0; 3]);
        for (i, b) in self.formants.iter().enumerate() {
            bands.f[i] = b.f.clamp(40.0, 12_000.0);
            bands.bw[i] = b.bw.clamp(20.0, 4000.0);
            bands.gain[i] = b.gain.clamp(0.0, 4.0);
        }
        bands.count = self.formants.len();
        Ok(ModelTarget {
            harmonics: std::array::from_fn(|i| self.harmonics.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.5)),
            bands,
            vox_mix: self.vox_mix.clamp(0.0, 1.0),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfileJson {
    pub f0: f32,
    pub harm_valid: bool,
    /// Harmonic levels by number, dB, energy-normalised.
    pub harmonics_db: Vec<f32>,
    pub env_valid: bool,
    /// Envelope on the 96-point log grid 40 Hz–16 kHz, dB, energy-normalised.
    pub envelope_db: Vec<f32>,
    pub noise: f32,
}

impl ProfileJson {
    pub fn from_profile(p: &TimbreProfile) -> Self {
        Self {
            f0: p.f0,
            harm_valid: p.harm_valid,
            harmonics_db: p.harm_db.to_vec(),
            env_valid: p.env_valid,
            envelope_db: p.env_db.to_vec(),
            noise: p.noise,
        }
    }
    pub fn to_profile(&self) -> Result<TimbreProfile, String> {
        if self.harmonics_db.len() != MAX_HARM || self.envelope_db.len() != GRID {
            return Err(format!("captured: harmonics_db[{MAX_HARM}] and envelope_db[{GRID}]"));
        }
        if self.harmonics_db.iter().chain(&self.envelope_db).any(|x| !x.is_finite()) || !self.f0.is_finite() {
            return Err("captured: non-finite value".into());
        }
        Ok(TimbreProfile {
            f0: self.f0,
            harm_valid: self.harm_valid,
            harm_db: std::array::from_fn(|i| self.harmonics_db[i]),
            env_valid: self.env_valid,
            env_db: std::array::from_fn(|i| self.envelope_db[i]),
            noise: self.noise.clamp(0.0, 1.0),
        })
    }
}

pub const STATE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
pub struct MorphState {
    pub version: u32,
    pub params: Vec<f32>,
    pub bypass: bool,
    #[serde(default)]
    pub active_preset: u32,
    pub model: ModelJson,
    #[serde(default)]
    pub captured: Option<ProfileJson>,
}
