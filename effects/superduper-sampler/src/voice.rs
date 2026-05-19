//! Sample-playback voice. One voice = one MIDI note in flight,
//! reading through the active sample at the right pitch ratio.

use std::sync::Arc;
use superduper_synth_core::dsp_blocks::{AdsrEnvelope, AdsrParams};
use crate::bank::SampleData;

pub const NOTE_FREE: u8 = 0xff;

#[derive(Clone)]
pub struct SampleVoice {
    pub key: u8,
    pub note_id: i32,
    pub velocity: f32,
    pub age_stamp: u64,
    /// Per-voice fractional read position in frames.
    pub frame_pos: f64,
    pub env: AdsrEnvelope,
    /// Cached pitch ratio so the audio thread doesn't recompute it
    /// per sample. Filled in on trigger.
    pub pitch_ratio: f64,
    /// Cached active-sample reference. Held until the voice releases.
    pub sample: Arc<SampleData>,
}

impl Default for SampleVoice {
    fn default() -> Self {
        Self {
            key: NOTE_FREE,
            note_id: -1,
            velocity: 0.0,
            age_stamp: 0,
            frame_pos: 0.0,
            env: AdsrEnvelope::default(),
            pitch_ratio: 1.0,
            sample: crate::bank::empty_sample(),
        }
    }
}

#[derive(Copy, Clone)]
pub struct VoiceParams {
    pub host_sr: f32,
    pub root_key: f32,
    pub tune_st: f32,
    pub fine_cents: f32,
    pub loop_on: bool,
    pub loop_start_frac: f32,
    pub loop_end_frac: f32,
    /// Playback trim — only frames between `trim_start_frac` and
    /// `trim_end_frac` (both as fractions of total length) are read.
    pub trim_start_frac: f32,
    pub trim_end_frac: f32,
    pub env: AdsrParams,
    pub output_lin: f32,
}

impl SampleVoice {
    pub fn is_idle(&self) -> bool {
        self.key == NOTE_FREE && self.env.is_idle()
    }

    pub fn gate_on(
        &mut self,
        key: u8,
        velocity: f32,
        note_id: i32,
        age_stamp: u64,
        sample: Arc<SampleData>,
        params: VoiceParams,
    ) {
        self.key = key;
        self.note_id = note_id;
        self.velocity = velocity;
        self.age_stamp = age_stamp;
        // Start playback at trim_start. Trimming the front of a
        // sample is the most common use — drop the silence on an
        // 808 hit, snap straight to the transient, etc.
        let total = sample.frame_count() as f64;
        let start = (params.trim_start_frac.clamp(0.0, 0.99) as f64) * total;
        self.frame_pos = start;
        self.pitch_ratio = compute_pitch_ratio(
            key as f32, params.root_key, params.tune_st, params.fine_cents,
            params.host_sr, sample.sample_rate as f32,
        );
        self.sample = sample;
        self.env.gate_on();
    }

    pub fn gate_off(&mut self) {
        self.env.gate_off();
    }

    /// Render one stereo sample.
    pub fn process(&mut self, params: VoiceParams) -> (f32, f32) {
        if self.is_idle() { return (0.0, 0.0); }
        let env_level = self.env.process(params.env);
        if env_level < 1e-5 && self.env.is_idle() {
            self.key = NOTE_FREE;
            return (0.0, 0.0);
        }
        let (l, r) = self.sample.read_stereo_lerp(self.frame_pos);
        self.frame_pos += self.pitch_ratio;
        // Loop / end handling — playback is constrained to the trim
        // range, then loop start/end live INSIDE that trim.
        let total = self.sample.frame_count() as f64;
        let trim_lo = (params.trim_start_frac.clamp(0.0, 0.99) as f64) * total;
        let trim_hi = (params.trim_end_frac.clamp(0.0, 1.0) as f64) * total;
        let trim_hi = trim_hi.max(trim_lo + 1.0).min(total);
        if params.loop_on && total > 1.0 {
            let lo = (params.loop_start_frac.clamp(0.0, 0.99) as f64) * total;
            let hi = (params.loop_end_frac.clamp(0.0, 1.0) as f64) * total;
            // Clamp loop points inside the trim window so the user
            // can't accidentally loop into silence outside trim.
            let lo = lo.max(trim_lo).min(trim_hi - 1.0);
            let hi = hi.max(lo + 1.0).min(trim_hi);
            if self.frame_pos >= hi {
                self.frame_pos = lo + (self.frame_pos - hi);
            }
        } else if self.frame_pos >= trim_hi {
            // End of trim range → release the voice cleanly via env.
            self.env.gate_off();
        }
        let g = env_level * self.velocity * params.output_lin;
        (l * g, r * g)
    }
}

#[inline]
fn compute_pitch_ratio(
    key: f32, root_key: f32, tune_st: f32, fine_cents: f32,
    host_sr: f32, sample_sr: f32,
) -> f64 {
    let semis = (key - root_key) + tune_st + fine_cents / 100.0;
    let pitch = 2f32.powf(semis / 12.0);
    // Sample-rate ratio so a 44.1 k WAV plays at the right pitch on a
    // 48 k host.
    let sr_ratio = sample_sr.max(1.0) / host_sr.max(1.0);
    (pitch * sr_ratio) as f64
}
