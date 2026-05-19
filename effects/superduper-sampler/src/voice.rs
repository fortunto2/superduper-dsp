//! Sample-playback voice. One voice = one MIDI note in flight,
//! reading through the active sample at the right pitch ratio.

use std::sync::Arc;
use superduper_synth_core::dsp_blocks::{
    AdsrEnvelope, AdsrParams, SvfFilter, SvfMode,
};
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
    /// per sample. Filled in on trigger. Stored unsigned — direction
    /// of motion is in `direction`.
    pub pitch_ratio: f64,
    /// +1.0 = forward (normal), -1.0 = reverse. Captured at trigger
    /// time so a mid-note tweak of the Reverse param doesn't flip the
    /// already-playing voice (which would jump-cut audibly).
    pub direction: f64,
    /// Cached active-sample reference. Held until the voice releases.
    pub sample: Arc<SampleData>,
    /// One filter per channel — stateful, kept across the voice's
    /// lifetime so cutoff modulation glides instead of jumping. Reset
    /// in `gate_on` so a freshly-stolen voice doesn't ring the prior
    /// note's tail through the filter.
    pub filter_l: SvfFilter,
    pub filter_r: SvfFilter,
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
            direction: 1.0,
            sample: crate::bank::empty_sample(),
            filter_l: SvfFilter::default(),
            filter_r: SvfFilter::default(),
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
    /// True = play backwards (Trim End → Trim Start). Snapshotted at
    /// trigger; not re-checked per sample. Loop is forced off in reverse.
    pub reverse: bool,
    /// Filter mode (None disables filtering entirely).
    pub filter_mode: Option<SvfMode>,
    /// Base cutoff in Hz, before env/velocity modulation.
    pub cutoff_hz: f32,
    pub resonance: f32,
    /// Semitones of cutoff modulation per unit of envelope level.
    /// Positive = brighter on attack, negative = darker on attack.
    pub env_to_cutoff_st: f32,
    /// Velocity-to-amp depth: 0 = ignore velocity (always full),
    /// 1 = full velocity scaling.
    pub vel_to_amp: f32,
    /// Velocity-to-cutoff in semitones at velocity = 1.0. Negative
    /// makes harder hits darker (rare but musical).
    pub vel_to_cutoff_st: f32,
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
        let total = sample.frame_count() as f64;
        let trim_lo = (params.trim_start_frac.clamp(0.0, 0.99) as f64) * total;
        let trim_hi = (params.trim_end_frac.clamp(0.0, 1.0) as f64) * total;
        let trim_hi = trim_hi.max(trim_lo + 1.0).min(total);
        // Reverse: start at the right edge of the trim window and read
        // backwards. Forward: start at trim_start so the transient hits
        // immediately. Direction is locked in here — flipping Reverse
        // mid-note doesn't re-direct an already-playing voice.
        if params.reverse {
            self.frame_pos = trim_hi - 1.0;
            self.direction = -1.0;
        } else {
            self.frame_pos = trim_lo;
            self.direction = 1.0;
        }
        self.pitch_ratio = compute_pitch_ratio(
            key as f32, params.root_key, params.tune_st, params.fine_cents,
            params.host_sr, sample.sample_rate as f32,
        );
        self.sample = sample;
        // Reset filter integrators on a fresh note so a voice-stolen
        // slot doesn't ring the previous note's tail through.
        self.filter_l.reset();
        self.filter_r.reset();
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
        let total = self.sample.frame_count() as f64;
        let trim_lo = (params.trim_start_frac.clamp(0.0, 0.99) as f64) * total;
        let trim_hi = (params.trim_end_frac.clamp(0.0, 1.0) as f64) * total;
        let trim_hi = trim_hi.max(trim_lo + 1.0).min(total);

        // Read with direction-aware bounds. In forward we silence past
        // trim_hi, in reverse we silence past trim_lo. Either way the
        // cursor parks so no further reads wander outside the window.
        let in_bounds = if self.direction >= 0.0 {
            self.frame_pos < trim_hi && self.frame_pos >= trim_lo
        } else {
            self.frame_pos > trim_lo && self.frame_pos <= trim_hi
        };
        let (raw_l, raw_r) = if in_bounds {
            self.sample.read_stereo_lerp(self.frame_pos)
        } else {
            (0.0, 0.0)
        };

        // Filter post-read. Cutoff modulated by ENV → cutoff_st +
        // velocity → cutoff_st (both in semitones, summed). Filter
        // disabled (Off) bypasses the filter entirely so it doesn't
        // even cost the 4-mul SVF per sample.
        let (l, r) = if let Some(mode) = params.filter_mode {
            let mod_st = env_level * params.env_to_cutoff_st
                + self.velocity * params.vel_to_cutoff_st;
            let cutoff_modulated = params.cutoff_hz * 2f32.powf(mod_st / 12.0);
            let lf = self.filter_l.process(raw_l, mode, cutoff_modulated, params.resonance, params.host_sr);
            let rf = self.filter_r.process(raw_r, mode, cutoff_modulated, params.resonance, params.host_sr);
            (lf, rf)
        } else {
            (raw_l, raw_r)
        };

        // Advance with sign — direction is +/-1.0 so we go forwards
        // or backwards by `pitch_ratio` samples per audio sample.
        self.frame_pos += self.pitch_ratio * self.direction;

        // Loop / end handling. Loop currently only honoured in forward
        // playback; reversing a loop is musically rare and adds branch
        // complexity, so we skip it for now.
        if params.loop_on && !params.reverse && total > 1.0 {
            let lo = (params.loop_start_frac.clamp(0.0, 0.99) as f64) * total;
            let hi = (params.loop_end_frac.clamp(0.0, 1.0) as f64) * total;
            let lo = lo.max(trim_lo).min(trim_hi - 1.0);
            let hi = hi.max(lo + 1.0).min(trim_hi);
            if self.frame_pos >= hi {
                self.frame_pos = lo + (self.frame_pos - hi);
            }
        } else if self.direction >= 0.0 && self.frame_pos >= trim_hi {
            self.frame_pos = trim_hi;
            self.env.gate_off();
        } else if self.direction < 0.0 && self.frame_pos <= trim_lo {
            self.frame_pos = trim_lo;
            self.env.gate_off();
        }

        // Velocity → amp. vel_to_amp = 0 ignores velocity (always full);
        // = 1 scales amp by velocity linearly. Interpolate so partial
        // values give partial sensitivity.
        let vel_gain = 1.0 - params.vel_to_amp + params.vel_to_amp * self.velocity;
        let g = env_level * vel_gain * params.output_lin;
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
