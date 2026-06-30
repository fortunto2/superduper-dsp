//! Wavetable oscillator + voice for SuperDuper Wave.
//!
//! Engine shape:
//!   - Each preset declares two formula-based waveforms (`frame_a`, `frame_b`).
//!     At preset-load time we sample each into a `WT_SIZE`-long table and
//!     stash both as `Arc<[f32; WT_SIZE]>` (cheap to clone-by-handle).
//!   - The oscillator reads from `frame_a` and `frame_b` with linear inter-
//!     polation between sample slots, then linearly blends the two frames
//!     by the runtime `WT Pos` value.
//!   - A `WaveVoice` holds a small fan-out of these oscillators for unison
//!     (up to UNISON_MAX detuned voices, panned across the stereo field),
//!     a sub-octave oscillator, a TPT/ZDF SVF lowpass and an ADSR envelope.
//!
//! Everything in the audio thread is RT-safe: no allocation, no locks. The
//! wavetable swap is just an `Arc` pointer swap, done from the main thread
//! after a preset change is queued (see `lib.rs`).

use std::sync::Arc;
use crate::analysis::lowpass_to_harmonics;
use crate::dsp_blocks::{AdsrEnvelope, AdsrParams};

/// Wavetable resolution. Powers of two are friendliest for the
/// modulo-by-bitmask trick. 2048 ≈ -100 dB harmonic floor for sub-Nyquist
/// content on a typical bass note, low enough that humans don't hear it.
pub const WT_SIZE: usize = 2048;
const WT_MASK: usize = WT_SIZE - 1;

/// Max simultaneous unison voices per note.  Anything past 7 turns into
/// noise (phase cancellation) for a single key — 5 is the sweet spot for
/// fat bass without flam.
pub const UNISON_MAX: usize = 7;

/// A single time-domain wavetable buffer — one bandwidth slice.
pub type Wavetable = Arc<[f32; WT_SIZE]>;

/// Number of mip levels (band-limited copies of the table) used for the
/// anti-aliased oscillator path.  Level k holds the first `WT_SIZE/2 / 2^k`
/// harmonics — level 0 = full bandwidth (good for sub-bass), level
/// `MIP_LEVELS-1` ≈ 2 harmonics (safe up to Nyquist on the highest note
/// the user can play).
pub const MIP_LEVELS: usize = 10;

/// Mip-mapped wavetable: an array of `MIP_LEVELS` increasingly low-passed
/// versions of the same waveform. Cheap to share between voices and across
/// the crossfade slot (the levels themselves are `Arc`'d).
#[derive(Clone)]
pub struct MipWavetable {
    pub levels: [Wavetable; MIP_LEVELS],
}

impl MipWavetable {
    /// Pick the appropriate mip level for a given fundamental Hz at sr.
    /// Returns 0 (full bandwidth) when `antialias_on` is false.
    ///
    /// Mip level `k` keeps `WT_SIZE/2 >> k` harmonics. To avoid folding,
    /// that ceiling must stay at/under the number of harmonics that fit
    /// below Nyquist, which is `(WT_SIZE/2) / ratio` where
    /// `ratio = WT_SIZE·freq/sr`. So we need `2^k >= ratio`, i.e.
    /// `k = ceil(log2(ratio))`. `ceil` is the TIGHTEST safe level — it
    /// keeps the most harmonics that *don't* alias.
    ///
    /// (This used `floor` for a while, to keep ~2× the harmonics for extra
    /// "detail". But floor's level aliases the whole boundary octave on
    /// every note above ~C2, and the loudest aliased harmonic survives —
    /// so anti-alias ON measured no quieter than OFF (see antialias_audit).
    /// That brightness was aliasing, not detail. For genuine per-note
    /// detail without aliasing, blend the two adjacent levels by the
    /// fractional part of log2(ratio) — a future upgrade; ceil is the
    /// correct floor-of-quality.)
    #[inline]
    pub fn pick_level(&self, freq_hz: f32, sr: f32, antialias_on: bool) -> usize {
        if !antialias_on {
            return 0;
        }
        let ratio = WT_SIZE as f32 * freq_hz.max(1.0) / sr;
        let level = ratio.log2().ceil().max(0.0) as usize;
        level.min(MIP_LEVELS - 1)
    }
}

/// Render a single bandwidth slice of a formula into a [`Wavetable`].
/// Caller is responsible for normalisation if the formula overshoots.
fn render_one(f: fn(f32) -> f32) -> Box<[f32; WT_SIZE]> {
    let mut buf = Box::new([0.0_f32; WT_SIZE]);
    for (i, slot) in buf.iter_mut().enumerate() {
        let phase = i as f32 / WT_SIZE as f32;
        *slot = f(phase);
    }
    let peak = buf.iter().map(|x| x.abs()).fold(0.0_f32, f32::max);
    if peak > 1.0 {
        let scale = 1.0 / peak;
        for s in buf.iter_mut() {
            *s *= scale;
        }
    }
    buf
}

/// Render a formula `f(phase) → amplitude` into a single full-bandwidth
/// wavetable.  Kept for tests + the curve-editor preview path; production
/// playback should use [`render_formula_mip`] so the engine has a mip pyramid
/// to pick from per-voice.
pub fn render_formula(f: fn(f32) -> f32) -> Wavetable {
    Arc::from(render_one(f))
}

/// Same as [`render_formula`] but builds the full mip pyramid (one FFT-based
/// low-pass per extra level).  Cost is ~MIP_LEVELS × `lowpass_to_harmonics`
/// FFTs — small enough to run on the GUI thread per preset / curve edit.
pub fn render_formula_mip(f: fn(f32) -> f32) -> MipWavetable {
    let base = render_one(f);
    mip_from_table(&base[..])
}

/// Build the mip pyramid from an arbitrary base table (used by the
/// custom-curve editor — it already has a full-bandwidth table baked from
/// the user's nodes and just needs the band-limited siblings).
pub fn mip_from_table(base: &[f32]) -> MipWavetable {
    debug_assert_eq!(base.len(), WT_SIZE);
    let mut levels: Vec<Wavetable> = Vec::with_capacity(MIP_LEVELS);
    // Level 0 = the base, full bandwidth.
    let mut full = Box::new([0.0_f32; WT_SIZE]);
    full.copy_from_slice(base);
    levels.push(Arc::from(full));
    // Each subsequent level halves the harmonic ceiling.
    for k in 1..MIP_LEVELS {
        let max_h = ((WT_SIZE / 2) >> k).max(1);
        let filtered = lowpass_to_harmonics(base, max_h);
        let mut buf = Box::new([0.0_f32; WT_SIZE]);
        buf.copy_from_slice(&filtered);
        levels.push(Arc::from(buf));
    }
    // We pushed exactly MIP_LEVELS — try_into is infallible.
    MipWavetable {
        levels: levels.try_into().unwrap_or_else(|_| panic!("mip-level count mismatch")),
    }
}

/// Public re-export for the `wave-pitch-bench` CLI (test-only). The
/// production playback path inlines `read_single` directly.
#[doc(hidden)]
pub fn read_single_for_test(table: &[f32; WT_SIZE], phase: f32) -> f32 {
    read_single(table, phase)
}

/// Single 4-point Lagrange (3rd-order) interp read from one wavetable.
///
/// Linear interp (the previous implementation) reads two adjacent
/// samples and lerps — fine for low-bandwidth content but cuts high
/// harmonics by ~6 dB per octave above sr/(2·N) when reading at
/// fractional positions. Lagrange-3 spans 4 samples and is flat to
/// within ~0.3 dB at high frequencies, so subtle wavetable detail
/// (harmonics 50-500) stays audible even when the read phase doesn't
/// line up with integer table indices.
#[inline]
fn read_single(table: &[f32; WT_SIZE], phase: f32) -> f32 {
    let p = phase.fract().max(0.0);
    let scaled = p * WT_SIZE as f32;
    let i1 = scaled as usize & WT_MASK;
    let i0 = (i1 + WT_SIZE - 1) & WT_MASK;
    let i2 = (i1 + 1) & WT_MASK;
    let i3 = (i1 + 2) & WT_MASK;
    let frac = scaled - (scaled as usize) as f32;
    let y0 = table[i0];
    let y1 = table[i1];
    let y2 = table[i2];
    let y3 = table[i3];
    // 4-point Lagrange polynomial through equally-spaced samples
    // (-1, 0, 1, 2). Coefficients pre-derived; runs in 4 mul + 3 add
    // per output.
    let f = frac;
    let c0 = -f * (f - 1.0) * (f - 2.0) / 6.0;
    let c1 = (f + 1.0) * (f - 1.0) * (f - 2.0) / 2.0;
    let c2 = -(f + 1.0) * f * (f - 2.0) / 2.0;
    let c3 = (f + 1.0) * f * (f - 1.0) / 6.0;
    y0 * c0 + y1 * c1 + y2 * c2 + y3 * c3
}

/// Sample N wavetable frames at `phase` and lerp between the
/// integer-adjacent pair selected by `wt_pos × (N-1)`. The first
/// frame (index 0) is read with a crossfade from `prev_a_table`
/// so live edits don't click-swap. Other frames are read directly.
#[inline]
fn read_morph(
    prev_a_table: &[f32; WT_SIZE],
    tables: &[&[f32; WT_SIZE]],
    phase: f32,
    fade: f32,
    wt_pos: f32,
) -> f32 {
    let n = tables.len();
    if n == 0 {
        return 0.0;
    }
    if n == 1 {
        let prev_s = read_single(prev_a_table, phase);
        let cur_s = read_single(tables[0], phase);
        return prev_s * (1.0 - fade) + cur_s * fade;
    }
    let pos = wt_pos.clamp(0.0, 1.0) * (n as f32 - 1.0);
    let i0 = (pos.floor() as usize).min(n - 1);
    let i1 = (i0 + 1).min(n - 1);
    let frac = pos - i0 as f32;
    let s_i0 = if i0 == 0 {
        let prev_s = read_single(prev_a_table, phase);
        let cur_s = read_single(tables[0], phase);
        prev_s * (1.0 - fade) + cur_s * fade
    } else {
        read_single(tables[i0], phase)
    };
    let s_i1 = read_single(tables[i1], phase);
    s_i0 * (1.0 - frac) + s_i1 * frac
}

/// One unison oscillator instance — just a phase accumulator.
#[derive(Clone, Copy)]
struct UnisonOsc {
    phase: f32,
    /// Per-voice detune ratio (1.0 = no detune).
    detune_ratio: f32,
    /// Constant-power pan: (left_gain, right_gain).
    pan_l: f32,
    pan_r: f32,
}

impl Default for UnisonOsc {
    fn default() -> Self {
        Self {
            phase: 0.0,
            detune_ratio: 1.0,
            pan_l: 0.7071,
            pan_r: 0.7071,
        }
    }
}

/// Sub-oscillator: pure sine, one octave below the played note. Phase keeps
/// continuity across notes (don't reset) so re-triggers don't click.
#[derive(Clone, Copy, Default)]
struct SubOsc {
    phase: f32,
}

/// Cheap white-noise generator — xorshift32 → uniform float in [-1, 1].
/// Per-voice so two simultaneous notes don't share the same random walk
/// (which would phase-cancel into stereo bleed).
#[derive(Clone, Copy)]
struct NoiseGen {
    state: u32,
}
impl Default for NoiseGen {
    fn default() -> Self {
        // Non-zero seed required by xorshift; mix per-voice to decorrelate.
        Self { state: 0x1ED5_BABE }
    }
}
impl NoiseGen {
    #[inline]
    fn next(&mut self) -> f32 {
        self.state ^= self.state << 13;
        self.state ^= self.state >> 17;
        self.state ^= self.state << 5;
        // Map upper 24 bits to a centred f32.
        let bits = (self.state >> 8) & 0x00FF_FFFF;
        (bits as f32 / 8_388_608.0) - 1.0
    }
}

impl SubOsc {
    #[inline]
    fn process(&mut self, freq_hz: f32, sr: f32) -> f32 {
        self.phase += freq_hz / sr;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        (self.phase * core::f32::consts::TAU).sin()
    }
}

/// TPT/ZDF state-variable filter — same kernel as PadVoice but exposed
/// with explicit LP/HP mode select. Numerically stable up to Nyquist
/// (Chamberlin SVF would blow up past sr/6).
#[derive(Clone, Copy, Default)]
pub struct SvfFilter {
    z1: f32,
    z2: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterMode {
    LowPass,
    HighPass,
    BandPass,
}

impl FilterMode {
    pub fn from_index(i: u32) -> Self {
        match i {
            1 => Self::HighPass,
            2 => Self::BandPass,
            _ => Self::LowPass,
        }
    }
}

impl SvfFilter {
    #[inline]
    pub fn process(&mut self, x: f32, sr: f32, cutoff_hz: f32, resonance: f32, mode: FilterMode) -> f32 {
        let cutoff = cutoff_hz.clamp(20.0, sr * 0.49);
        let g = (core::f32::consts::PI * cutoff / sr).tan();
        let k = 2.0 - 2.0 * resonance.clamp(0.0, 0.95);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let v1 = a1 * (self.z1 + g * (x - self.z2));
        let lowpass = self.z2 + g * v1;
        let highpass = x - k * v1 - self.z2 - g * v1;
        let bandpass = v1;
        self.z1 = 2.0 * v1 - self.z1;
        self.z2 = 2.0 * lowpass - self.z2;
        match mode {
            FilterMode::LowPass => lowpass,
            FilterMode::HighPass => highpass,
            FilterMode::BandPass => bandpass,
        }
    }
}

/// One playable voice. Holds the unison fan-out, sub-osc, filter and ADSR.
/// One `WaveVoice` per active MIDI note.
#[derive(Clone)]
pub struct WaveVoice {
    osc: [UnisonOsc; UNISON_MAX],
    sub: SubOsc,
    noise: NoiseGen,
    filter_l: SvfFilter,
    filter_r: SvfFilter,
    pub env: AdsrEnvelope,
    pub filter_env: AdsrEnvelope,
    /// Per-voice LFO phase ∈ [0, 1). Free-running, only resets on note-on.
    pub lfo_phase: f32,
    pub key: u8,
    pub note_id: i32,
    pub velocity: f32,
    pub age_stamp: u64,
    /// Choke-fade state — same pattern as Pad, fade out 5 ms on NoteChoke /
    /// CC 120 to avoid the hard-cut click.
    pub choke_remaining: u32,
    pub choke_total: u32,
    pub choke_level: f32,
    /// Phantom master phase used by Hard Sync — when this crosses 1.0
    /// the main unison-osc phases are reset to 0, producing the
    /// classic "sync sweep" sound.
    pub sync_phase: f32,
    /// Independent phase used by the cross-FM modulator (sine at
    /// root × FM Ratio). Sampled per-sample to phase-modulate the
    /// main wavetable read.
    pub fm_phase: f32,
}

pub const NOTE_FREE: u8 = 0xff;

impl Default for WaveVoice {
    fn default() -> Self {
        Self {
            osc: [UnisonOsc::default(); UNISON_MAX],
            sub: SubOsc::default(),
            noise: NoiseGen::default(),
            filter_l: SvfFilter::default(),
            filter_r: SvfFilter::default(),
            env: AdsrEnvelope::default(),
            filter_env: AdsrEnvelope::default(),
            lfo_phase: 0.0,
            key: NOTE_FREE,
            note_id: -1,
            velocity: 0.0,
            age_stamp: 0,
            choke_remaining: 0,
            choke_total: 0,
            choke_level: 0.0,
            sync_phase: 0.0,
            fm_phase: 0.0,
        }
    }
}

/// LFO waveform shape selector. From integer P_LFO_SHAPE param.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LfoShape {
    Sine,
    Triangle,
    Saw,
    Square,
}
impl LfoShape {
    pub fn from_index(i: u32) -> Self {
        match i {
            1 => Self::Triangle,
            2 => Self::Saw,
            3 => Self::Square,
            _ => Self::Sine,
        }
    }
}

/// LFO modulation destination. Picks which knob the LFO sums into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LfoDest {
    /// Modulate filter cutoff (in octaves).
    Cutoff,
    /// Modulate oscillator pitch (in semitones).
    Pitch,
    /// Modulate wavetable WT Pos (linear).
    WtPos,
}
impl LfoDest {
    pub fn from_index(i: u32) -> Self {
        match i {
            1 => Self::Pitch,
            2 => Self::WtPos,
            _ => Self::Cutoff,
        }
    }
}

/// Parameters passed per render call.
#[derive(Copy, Clone)]
pub struct WaveParams<'a> {
    pub sr: f32,
    pub root_hz: f32,
    pub wt_pos: f32,
    pub unison: u32,
    pub detune_cents: f32,
    pub sub_level: f32,
    pub noise_level: f32,
    pub cutoff_hz: f32,
    pub resonance: f32,
    pub mode: FilterMode,
    pub drive: f32,
    /// Anti-alias toggle. When off, the engine always reads mip level 0
    /// (full bandwidth) so you can A/B against the band-limited path.
    pub antialias: bool,
    /// Volume-envelope params (ADSR drives amp; caller multiplies output).
    /// The voice itself only owns the second ADSR (filter env) — vol env is
    /// kept outside so callers can re-use the same instance across blocks.
    pub fenv_amount_oct: f32,
    pub fenv: AdsrParams,
    /// LFO params.
    pub lfo_shape: LfoShape,
    pub lfo_dest: LfoDest,
    pub lfo_rate_hz: f32,
    pub lfo_depth: f32,
    /// All wavetable frames (1..=`FRAMES_MAX`). WT Pos × (N-1) picks
    /// an integer pair to blend across — fractional part is the lerp.
    /// First entry (frames[0]) is the live edit target.
    pub frames: &'a [MipWavetable],
    /// The previous frames[0] — read in crossfade with the live one
    /// for the first ~12 ms after a curve push, killing zipper noise
    /// from rapid mouse-drag wavetable edits.
    pub frame_a_prev: &'a MipWavetable,
    pub frame_a_fade: f32,

    // ---- Hard sync + cross-FM ----
    pub sync_on: bool,
    pub sync_ratio: f32,
    pub fm_ratio: f32,
    pub fm_amount: f32,

    // ---- Mod-matrix slot snapshots (caller fills both — 2 slots) ----
    pub mod_slots: [ModSlot; 2],
    /// Per-block global modulator source values, evaluated by the caller
    /// once before iterating voices. LFO + Velocity + FEnv are NOT here —
    /// they're per-voice and the voice computes them itself.
    pub mod_wheel: f32,
    pub aftertouch: f32,
}

/// One mod-matrix slot — src/dst are decoded into typed enums in lib.rs.
#[derive(Copy, Clone, Default)]
pub struct ModSlot {
    pub src: ModSource,
    pub dst: ModDest,
    pub amt: f32,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum ModSource {
    #[default]
    None,
    Lfo,
    Velocity,
    ModWheel,
    Aftertouch,
    FilterEnv,
}
impl ModSource {
    pub fn from_index(i: u32) -> Self {
        match i {
            1 => Self::Lfo,
            2 => Self::Velocity,
            3 => Self::ModWheel,
            4 => Self::Aftertouch,
            5 => Self::FilterEnv,
            _ => Self::None,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum ModDest {
    #[default]
    None,
    CutoffOct,
    PitchSt,
    WtPos,
    Resonance,
    Drive,
    Volume,
}
impl ModDest {
    pub fn from_index(i: u32) -> Self {
        match i {
            1 => Self::CutoffOct,
            2 => Self::PitchSt,
            3 => Self::WtPos,
            4 => Self::Resonance,
            5 => Self::Drive,
            6 => Self::Volume,
            _ => Self::None,
        }
    }
}

/// Per-voice modulator accumulator. One slot per destination.
#[derive(Copy, Clone, Default)]
pub struct ModAccum {
    pub cutoff_oct: f32,
    pub pitch_st: f32,
    pub wt_pos: f32,
    pub resonance: f32,
    pub drive: f32,
    pub volume: f32,
}

impl WaveVoice {
    /// Configure the unison spread: detune in cents and equal-power pan.
    /// Called at note-on so re-triggering a held key doesn't re-randomise
    /// the voice layout.
    pub fn configure_unison(&mut self, count: u32, detune_cents: f32) {
        let n = (count as usize).clamp(1, UNISON_MAX);
        for i in 0..UNISON_MAX {
            if i >= n {
                // Unused slots get zero pan + unity detune so they stay
                // dormant if `process` ever touches them.
                self.osc[i].detune_ratio = 1.0;
                self.osc[i].pan_l = 0.0;
                self.osc[i].pan_r = 0.0;
                continue;
            }
            // Symmetric detune: voice i centred at -1..+1 → cents = ±detune.
            let mid = (n as f32 - 1.0) * 0.5;
            let offset = if mid > 0.0 { (i as f32 - mid) / mid } else { 0.0 };
            let cents = offset * detune_cents;
            self.osc[i].detune_ratio = 2f32.powf(cents / 1200.0);
            // Equal-power pan: leftmost voice fully L, rightmost fully R, mid centred.
            let pan = offset; // -1..+1
            let angle = (pan + 1.0) * 0.25 * core::f32::consts::PI;
            self.osc[i].pan_l = angle.cos();
            self.osc[i].pan_r = angle.sin();
        }
    }

    /// Optional: scatter starting phases so unison voices don't all start
    /// in lockstep (prevents a small initial transient on first note).
    /// Only scatters slots that are still at phase = 0.
    pub fn scatter_phases(&mut self) {
        // Deterministic golden-ratio walk.
        let phi = 0.6180339887_f32;
        let mut p = 0.13_f32;
        for o in self.osc.iter_mut() {
            if o.phase == 0.0 {
                o.phase = p;
            }
            p = (p + phi).fract();
        }
    }

    /// Render one stereo sample from this voice. Returns (L, R).
    #[inline]
    pub fn process(&mut self, p: WaveParams<'_>) -> (f32, f32) {
        // ---- Modulation sources: LFO + filter envelope ----
        self.lfo_phase += p.lfo_rate_hz / p.sr;
        if self.lfo_phase >= 1.0 {
            self.lfo_phase -= 1.0;
        }
        let lfo_raw = match p.lfo_shape {
            LfoShape::Sine => (self.lfo_phase * core::f32::consts::TAU).sin(),
            LfoShape::Triangle => {
                if self.lfo_phase < 0.5 {
                    4.0 * self.lfo_phase - 1.0
                } else {
                    3.0 - 4.0 * self.lfo_phase
                }
            }
            LfoShape::Saw => 2.0 * self.lfo_phase - 1.0,
            LfoShape::Square => {
                if self.lfo_phase < 0.5 { 1.0 } else { -1.0 }
            }
        };
        let lfo_value = lfo_raw * p.lfo_depth;
        let fenv_level = self.filter_env.process(p.fenv);

        // ---- Mod matrix: evaluate each configured slot and accumulate
        // its contribution into the per-voice destination accumulators.
        // Source values are in roughly [-1, 1] (LFO + Velocity bias to 0
        // when off-key; ModWheel/Aftertouch/FEnv are 0..1). Destination
        // scaling is destination-specific — Cutoff: 12 ST = 1 octave per
        // unit amt; Pitch: 24 ST per unit amt; WtPos / Reson / Drive /
        // Vol: ±1 per unit amt.
        let mut acc = ModAccum::default();
        for slot in p.mod_slots.iter() {
            let src_v = match slot.src {
                ModSource::None => continue,
                ModSource::Lfo => lfo_value,                // ∈ [-1,1] × depth
                ModSource::Velocity => self.velocity,       // 0..1
                ModSource::ModWheel => p.mod_wheel,         // 0..1
                ModSource::Aftertouch => p.aftertouch,      // 0..1
                ModSource::FilterEnv => fenv_level,         // 0..1
            };
            let v = src_v * slot.amt;
            match slot.dst {
                ModDest::None => {}
                ModDest::CutoffOct => acc.cutoff_oct += v * 4.0,   // up to ±4 oct
                ModDest::PitchSt   => acc.pitch_st   += v * 24.0,  // up to ±24 ST
                ModDest::WtPos     => acc.wt_pos     += v,
                ModDest::Resonance => acc.resonance  += v,
                ModDest::Drive     => acc.drive      += v,
                ModDest::Volume    => acc.volume     += v,
            }
        }

        // ---- Per-sample modulated values ----
        // Filter env modulates cutoff in octaves (classic synth shape).
        // LFO can also be routed to cutoff (additive, in octaves).
        let cutoff_oct = p.fenv_amount_oct * fenv_level
            + if matches!(p.lfo_dest, LfoDest::Cutoff) { lfo_value } else { 0.0 }
            + acc.cutoff_oct;
        let cutoff_hz = (p.cutoff_hz * 2f32.powf(cutoff_oct)).clamp(20.0, p.sr * 0.49);
        // LFO → pitch: depth measured in semitones (we scale value by 12
        // up front to make ±depth roughly ±12 ST at depth=1).
        let pitch_ratio = {
            let lfo_pitch_st = if matches!(p.lfo_dest, LfoDest::Pitch) {
                lfo_value * 12.0
            } else {
                0.0
            };
            2f32.powf((lfo_pitch_st + acc.pitch_st) / 12.0)
        };
        // LFO → WT Pos: additive, clamped.
        let wt_pos = {
            let base = if matches!(p.lfo_dest, LfoDest::WtPos) {
                p.wt_pos + lfo_value
            } else {
                p.wt_pos
            };
            (base + acc.wt_pos).clamp(0.0, 1.0)
        };
        let resonance = (p.resonance + acc.resonance).clamp(0.0, 0.99);
        let drive = (p.drive + acc.drive).clamp(0.0, 4.0);
        // Volume scaler — 1.0 baseline, +ve amt brightens, -ve mutes.
        // Clamp to keep voices from blowing up at extreme settings.
        let voice_gain = (1.0 + acc.volume).clamp(0.0, 4.0);

        let n_active = self.active_voice_count();
        let inv_n = 1.0 / (n_active as f32);

        let mut mix_l = 0.0_f32;
        let mut mix_r = 0.0_f32;
        // Main unison stack.
        // Hard sync — advance the phantom master sine; when it wraps
        // past 1.0 the main unison phases get reset on the very next
        // sample. We sample the wrap flag once per outer voice-step so
        // every unison member resets together (otherwise they'd phase-
        // smear). Sync stays inert when sync_ratio is exactly 1.0 OR
        // sync_on is false — kept as a single fast branch.
        let sync_reset = if p.sync_on {
            let sync_freq = p.root_hz * p.sync_ratio.max(0.01) * pitch_ratio;
            self.sync_phase += sync_freq / p.sr;
            let reset = self.sync_phase >= 1.0;
            if reset { self.sync_phase -= 1.0; }
            reset
        } else {
            false
        };

        // FM modulator — sine at root × FM Ratio. Output ∈ [-1, 1] is
        // scaled by fm_amount and added to the wavetable read phase
        // (modulo 1.0). Cheap, audibly musical phase modulation; no
        // separate oscillator block needed.
        let fm_sample = if p.fm_amount > 0.0001 {
            let fm_freq = p.root_hz * p.fm_ratio.max(0.01) * pitch_ratio;
            self.fm_phase += fm_freq / p.sr;
            if self.fm_phase >= 1.0 { self.fm_phase -= 1.0; }
            (self.fm_phase * core::f32::consts::TAU).sin() * p.fm_amount
        } else {
            0.0
        };

        for i in 0..n_active {
            let o = &mut self.osc[i];
            if sync_reset { o.phase = 0.0; }
            let voice_freq = p.root_hz * o.detune_ratio * pitch_ratio;
            let phase_inc = voice_freq / p.sr;
            o.phase += phase_inc;
            if o.phase >= 1.0 {
                o.phase -= 1.0;
            }
            // Phase-mod the wavetable read by the FM sample. Wraps
            // through 1.0 by the additive modulo down in read_morph.
            let read_phase = (o.phase + fm_sample).rem_euclid(1.0);
            // All frames share the same shape, so pick the mip level
            // off frames[0] — pick_level is a function of frequency,
            // not content.
            let frames = p.frames;
            let mip = frames[0].pick_level(voice_freq, p.sr, p.antialias);
            let table_a_prev = &p.frame_a_prev.levels[mip];
            // Build a stack-array of `&[f32; WT_SIZE]` refs for every
            // active frame at the chosen mip level. FRAMES_MAX is a
            // tight cap so this stays heap-free per sample.
            let n_frames = frames.len().min(16);
            let mut tables: [&[f32; WT_SIZE]; 16] = [&frames[0].levels[mip]; 16];
            for i in 1..n_frames {
                tables[i] = &frames[i].levels[mip];
            }
            let s = read_morph(table_a_prev, &tables[..n_frames], read_phase, p.frame_a_fade, wt_pos);
            mix_l += s * o.pan_l;
            mix_r += s * o.pan_r;
        }
        mix_l *= inv_n;
        mix_r *= inv_n;

        // Sub-oscillator (mono, mixed into both channels) — one octave down.
        if p.sub_level > 0.0001 {
            let sub = self.sub.process(p.root_hz * 0.5 * pitch_ratio, p.sr);
            mix_l += sub * p.sub_level * 0.7071;
            mix_r += sub * p.sub_level * 0.7071;
        }

        // White-noise mix — independent per channel so it widens the stereo
        // image instead of folding into mono.
        if p.noise_level > 0.0001 {
            mix_l += self.noise.next() * p.noise_level;
            mix_r += self.noise.next() * p.noise_level;
        }

        // Pre-filter drive — tanh waveshaper.
        if drive > 0.0001 {
            let g = 1.0 + drive * 2.5;
            mix_l = (mix_l * g).tanh();
            mix_r = (mix_r * g).tanh();
        }

        // Stereo filter (L and R have independent state — keeps spread audible).
        let out_l = self.filter_l.process(mix_l, p.sr, cutoff_hz, resonance, p.mode);
        let out_r = self.filter_r.process(mix_r, p.sr, cutoff_hz, resonance, p.mode);
        (out_l * voice_gain, out_r * voice_gain)
    }

    #[inline]
    fn active_voice_count(&self) -> usize {
        // Slots with non-zero pan are considered "in use" by the unison
        // configuration. configure_unison() zeroes the unused tail.
        self.osc.iter().take_while(|o| o.pan_l != 0.0 || o.pan_r != 0.0).count().max(1)
    }
}
