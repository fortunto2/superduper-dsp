//! SuperDuper Wind voice — deterministic additive tone + stochastic
//! noise "wind bed", the classic Spectral Modeling Synthesis (SMS) split:
//!
//!   deterministic: a handful of additive harmonics (brightness = Tone)
//!                  through a 3-band formant for vocal/flute colour.
//!   stochastic:    TWO noise layers cross-faded by `Howl`:
//!                  - the original gentle formant-bandpassed breath noise
//!                    (pink↔white via Color) — airy, close-mic'd breath.
//!                  - `howl::HowlEngine` — Andy Farnell's procedural
//!                    howling-wind model (broadband noise → 2-3 swept
//!                    high-Q resonant bandpasses, ~200 Hz-2 kHz) — the
//!                    pitched "whoooo" of actual howling wind («завывание»).
//!                  `Howl` morphs between them: 0 = pure gentle breath
//!                  instrument, 1 = dominant procedural howl. Combined
//!                  amplitude = Breath × envelope × (1 + Shimmer wobble)
//!                  × the caller's shared gust multiplier.
//!
//! Plus two performance-feel touches: Jitter (per-voice pitch wobble from
//! smoothed pink noise) and Chiff (a short breath-noise burst on note-on).
//! All noise sources are decorrelated per stereo channel so the wind bed
//! has real width instead of a mono-duplicated hiss. The howl engine's
//! sweep range is transposed by the played note (`root_hz`) so it's
//! genuinely "playable" — different notes howl at different pitches.

use crate::howl::HowlEngine;
use crate::noise::{ColorNoise, WobbleGen, Xorshift32};
use superduper_synth_core::dsp_blocks::{AdsrEnvelope, DcBlocker};
use superduper_synth_core::formant::Formant;

/// Fundamental + 5 overtones — enough to carry a recognisable pitch/timbre
/// without the CPU cost (or brittleness) of Kubyz's full 16-partial table;
/// a breath instrument's colour comes mostly from the formant + noise, not
/// from a dense harmonic stack.
pub const N_HARM: usize = 6;

/// Sentinel "no note" key value, matching the Kubyz/Wave/Pad convention.
pub const NOTE_FREE: u8 = 0xff;

/// Reference root frequency for the howl engine's `pitch_mult` — playing
/// this note (A3) leaves the 200 Hz-2 kHz sweep range at its base tuning.
const HOWL_PITCH_REF_HZ: f32 = 220.0;

/// Per-sample-block parameters handed to every voice. Small and Copy so
/// it's cheap to pass by value into the render loop.
#[derive(Copy, Clone)]
pub struct WindParams {
    pub sr: f32,
    pub root_hz: f32,
    pub harmonics: [f32; N_HARM],
    pub formant_f: [f32; 3],
    pub formant_bw: [f32; 3],
    pub formant_gain: [f32; 3],
    pub breath: f32,
    pub jitter: f32,
    pub shimmer: f32,
    pub chiff: f32,
    pub color: f32,
    /// 0 = pure gentle breath (old behaviour), 1 = dominant procedural
    /// howling wind. Also fades the additive tone down as it rises, so a
    /// full-Howl patch reads as wind, not flute.
    pub howl: f32,
    /// Shared gust-surge amplitude multiplier for the whole noise bed
    /// (tone excluded) — computed ONCE per block by the caller (one gust
    /// affects every voice uniformly, not independently per note). Also
    /// drives the Aeolian whistle's Strouhal wind speed `U`.
    pub gust_mult: f32,
    /// Aeolian-tone (vortex-shedding whistle) blend, 0..1 — 0 = pure
    /// broadband howl, 1 = strong whistle riding on top, gliding in pitch
    /// with `gust_mult`. Gated by `howl` (no whistle in the gentle-breath
    /// end of the Howl range) — see `howl::HowlEngine::process`.
    pub whistle: f32,
}

pub struct WindVoice {
    phases: [f32; N_HARM],
    tone_formant: Formant,
    noise_formant: Formant,
    howl_engine: HowlEngine,
    jitter_gen: WobbleGen,
    shimmer_gen: WobbleGen,
    noise_l: ColorNoise,
    noise_r: ColorNoise,
    chiff_l: Xorshift32,
    chiff_r: Xorshift32,
    dc_l: DcBlocker,
    dc_r: DcBlocker,

    pub env: AdsrEnvelope,
    pub key: u8,
    pub note_id: i32,
    pub velocity: f32,
    pub age_stamp: u64,

    // Deferred-steal choke-fade state — see lesson 17b in CLAUDE.md: a
    // stolen voice fades its OLD note to silence over a few ms instead of
    // snapping to the new pitch, and the new note is "parked" until the
    // fade completes so it always starts from silence (click-free).
    pub choke_remaining: u32,
    pub choke_total: u32,
    pub choke_level: f32,
    pub pending_key: u8,
    pub pending_note_id: i32,
    pub pending_velocity: f32,
    /// Set when a NoteOff arrives for a note that is still parked behind a
    /// choke-fade. Without it the release was compared only against `key` — which
    /// still holds the note being faded OUT — so the NoteOff was dropped and the
    /// parked note sounded forever, clearable only by CC 123/120.
    pub pending_released: bool,

    // Short on-note amplitude ramp (independent of the ADSR's own Attack —
    // kills the residual click even at Attack=0) and the chiff burst timer.
    note_fade_remaining: u32,
    note_fade_total: u32,
    chiff_remaining: u32,
    chiff_total: u32,
}

impl WindVoice {
    /// Build one voice slot. `idx` seeds every noise generator with a
    /// distinct prime-multiplied value so voices (and L/R within a voice)
    /// never sound like phase-locked copies of each other.
    pub fn new(idx: usize) -> Self {
        let base = (idx as u32).wrapping_mul(2_654_435_761).wrapping_add(1);
        Self {
            phases: [0.0; N_HARM],
            tone_formant: Formant::default(),
            noise_formant: Formant::default(),
            howl_engine: HowlEngine::new(base ^ 0x1F83_D9AB),
            jitter_gen: WobbleGen::new(base ^ 0x9E37_79B1),
            shimmer_gen: WobbleGen::new(base ^ 0x85EB_CA6B),
            noise_l: ColorNoise::new(base ^ 0xC2B2_AE35),
            noise_r: ColorNoise::new(base ^ 0x27D4_EB2F),
            chiff_l: Xorshift32::new(base ^ 0x1656_67B1),
            chiff_r: Xorshift32::new(base ^ 0xB531_A935),
            dc_l: DcBlocker::default(),
            dc_r: DcBlocker::default(),
            env: AdsrEnvelope::default(),
            key: NOTE_FREE,
            note_id: -1,
            velocity: 0.0,
            age_stamp: 0,
            choke_remaining: 0,
            choke_total: 0,
            choke_level: 0.0,
            pending_key: NOTE_FREE,
            pending_note_id: -1,
            pending_velocity: 0.0,
            pending_released: false,
            note_fade_remaining: 0,
            note_fade_total: 0,
            chiff_remaining: 0,
            chiff_total: 0,
        }
    }

    /// Called on every fresh NoteOn (not on legato retrigger). Scatters
    /// oscillator phases, arms the on-note fade, and fires the chiff burst.
    pub fn on_note_on(&mut self, sample_rate: f32) {
        let phi = 0.618_033_988_5_f32;
        let mut p = 0.137_f32;
        for ph in self.phases.iter_mut() {
            *ph = p;
            p = (p + phi).fract();
        }
        let fade_samples = (sample_rate * 0.002) as u32;
        self.note_fade_total = fade_samples.max(1);
        self.note_fade_remaining = self.note_fade_total;

        // ~50 ms breath-noise attack burst — the "chiff" of a tongued note.
        let chiff_samples = (sample_rate * 0.05) as u32;
        self.chiff_total = chiff_samples.max(1);
        self.chiff_remaining = self.chiff_total;
    }

    /// Deterministic tone + stochastic wind bed, mixed and formant-shaped.
    /// Does NOT apply the ADSR — the caller scales by `env * velocity` so
    /// per-voice envelope bookkeeping stays in one place (the render loop).
    #[inline]
    fn process_inner(&mut self, p: &WindParams) -> (f32, f32) {
        let sr = p.sr;
        let howl = p.howl.clamp(0.0, 1.0);

        // Jitter — smoothed-noise pitch wobble, up to ±40 cents at Jitter=1.
        let jitter_cents = self.jitter_gen.next(sr, 5.5) * 40.0 * p.jitter;
        let pitch_mult = 2f32.powf(jitter_cents / 1200.0);

        // Deterministic additive tone — fades out as Howl rises so a
        // full-Howl patch reads as wind, not flute, while never going
        // completely silent (keeps some pitched grounding).
        let mut tone = 0.0_f32;
        for n in 0..N_HARM {
            let amp = p.harmonics[n];
            if amp.abs() < 1e-5 {
                continue;
            }
            let inc = p.root_hz * pitch_mult * (n + 1) as f32 / sr;
            self.phases[n] += inc;
            if self.phases[n] >= 1.0 {
                self.phases[n] -= 1.0;
            }
            tone += amp * (self.phases[n] * core::f32::consts::TAU).sin();
        }
        let tone_gain = (1.0 - 0.9 * howl).max(0.08);
        // Soft-clip the additive sum — worst case (full in-phase alignment
        // of all 6 partials) is 6.0, realistically well under 2.0.
        let tone = (tone * 0.5).tanh() * tone_gain;
        let (tone_l, tone_r) = self
            .tone_formant
            .process(tone, tone, sr, p.formant_f, p.formant_bw, p.formant_gain, 1.0);

        // ---- Stochastic wind bed: gentle breath ↔ procedural howl -----
        let shimmer = self.shimmer_gen.next(sr, 4.5) * p.shimmer;
        let breath_amp = (p.breath * (1.0 + shimmer * 0.5)).max(0.0);

        let raw_l = self.noise_l.next(p.color);
        let raw_r = self.noise_r.next(p.color);
        let (old_breath_l, old_breath_r) = self.noise_formant.process(
            raw_l, raw_r, sr, p.formant_f, p.formant_bw, p.formant_gain, 1.0,
        );

        let howl_pitch_mult = (p.root_hz / HOWL_PITCH_REF_HZ).clamp(0.3, 3.5);
        let (howl_l, howl_r) = self
            .howl_engine
            .process(sr, howl, p.color, howl_pitch_mult, p.whistle, p.gust_mult);

        let old_weight = 1.0 - 0.85 * howl;
        let howl_weight = howl;
        let bed_scale = breath_amp * p.gust_mult;
        let bed_l = (old_breath_l * old_weight + howl_l * howl_weight) * bed_scale;
        let bed_r = (old_breath_r * old_weight + howl_r * howl_weight) * bed_scale;

        // Chiff — short broadband (un-formanted) breath burst on note-on.
        let (chiff_l, chiff_r) = if self.chiff_remaining > 0 {
            let fade = self.chiff_remaining as f32 / self.chiff_total as f32;
            let cl = self.chiff_l.next_bipolar() * fade;
            let cr = self.chiff_r.next_bipolar() * fade;
            self.chiff_remaining -= 1;
            (cl * p.chiff, cr * p.chiff)
        } else {
            (0.0, 0.0)
        };

        (tone_l + bed_l + chiff_l, tone_r + bed_r + chiff_r)
    }

    /// Public render entry point — wraps `process_inner` with the on-note
    /// ramp and a DC blocker (formant bandpass + tanh can leave a faint
    /// bias that otherwise reads as background rumble).
    #[inline]
    pub fn process(&mut self, p: &WindParams) -> (f32, f32) {
        let (mut l, mut r) = self.process_inner(p);
        if self.note_fade_remaining > 0 {
            let fade = 1.0 - (self.note_fade_remaining as f32) / (self.note_fade_total as f32);
            l *= fade;
            r *= fade;
            self.note_fade_remaining -= 1;
        }
        (self.dc_l.process(l), self.dc_r.process(r))
    }

    /// Last Strouhal-derived Aeolian whistle frequency (Hz) — telemetry for
    /// tests/GUI proving "the whistle glides with the gust", not used by
    /// the audio path itself.
    pub fn whistle_hz(&self) -> f32 {
        self.howl_engine.last_whistle_hz()
    }
}
