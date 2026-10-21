//! The playing half: voices, sustain pedal, release triggers, keyswitches,
//! CC-triggered regions, `off_by` / polyphony choking.
//!
//! RT contract: `note_on`, `note_off`, `cc`, `render` never allocate, lock or
//! panic. The voice pool is sized at construction. The engine borrows the
//! `Instrument` per call instead of owning it, so the caller decides how an
//! instrument swap is published (and where the old one gets dropped — never
//! on the audio thread, it can be a gigabyte).

use super::region::{InstrumentDef, LoopMode, ModTarget, Region, Trigger};
use crate::dsp_blocks::Xorshift;
use std::sync::Arc;

/// Decoded sample. Stored as i16: a piano library is ~1.3 GB this way and
/// would be 2.6 GB as f32; 16 bits is 96 dB of range, below any sample's own
/// noise floor.
#[derive(Debug)]
pub struct SampleBuf {
    pub frames: usize,
    /// 1 or 2.
    pub channels: usize,
    pub sample_rate: f32,
    /// Interleaved when stereo.
    pub data: Box<[i16]>,
}

impl SampleBuf {
    pub fn silent() -> Self {
        Self { frames: 0, channels: 1, sample_rate: 48_000.0, data: Box::new([]) }
    }

    /// Frame `i` as (left, right) in −1..1; out of range is silence.
    #[inline]
    pub fn frame(&self, i: isize) -> (f32, f32) {
        if i < 0 || i as usize >= self.frames { return (0.0, 0.0); }
        let i = i as usize;
        const S: f32 = 1.0 / 32768.0;
        if self.channels == 2 {
            match (self.data.get(2 * i), self.data.get(2 * i + 1)) {
                (Some(&l), Some(&r)) => (l as f32 * S, r as f32 * S),
                _ => (0.0, 0.0),
            }
        } else {
            let v = self.data.get(i).map_or(0.0, |&s| s as f32 * S);
            (v, v)
        }
    }
}

/// A playable instrument: compiled regions + their decoded samples.
#[derive(Debug, Default)]
pub struct Instrument {
    pub def: InstrumentDef,
    /// Parallel to `def.sample_paths`. A sample that failed to load is an
    /// empty buffer (its regions play silence) and is listed in `missing`.
    pub samples: Vec<Arc<SampleBuf>>,
    pub missing: Vec<String>,
    pub name: String,
}

#[inline]
fn hermite(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    let c = (x1 - xm1) * 0.5;
    let v = x0 - x1;
    let w = c + v;
    let a = w + v + (x2 - x0) * 0.5;
    let b = w + a;
    ((a * t - b) * t + c) * t + x0
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Stage { Delay, Attack, Hold, Decay, Sustain, Release, Done }

/// SFZ `ampeg_*`: linear attack, exponential decay and release. Release and
/// decay reach −80 dB at their nominal time, which is where a voice ends.
#[derive(Clone, Copy, Debug)]
struct Env {
    stage: Stage,
    level: f32,
    counter: u32,
    delay: u32,
    attack: u32,
    hold: u32,
    decay_coef: f32,
    sustain: f32,
    release_coef: f32,
}

const ENV_FLOOR: f32 = 1e-4;

fn exp_coef(seconds: f32, sr: f32) -> f32 {
    let n = (seconds.max(0.0) * sr).max(1.0);
    (ENV_FLOOR.ln() / n).exp()
}

impl Env {
    fn idle() -> Self {
        Self { stage: Stage::Done, level: 0.0, counter: 0, delay: 0, attack: 0, hold: 0, decay_coef: 0.0, sustain: 1.0, release_coef: 0.0 }
    }

    fn start(delay: f32, attack: f32, hold: f32, decay: f32, sustain: f32, release: f32, sr: f32) -> Self {
        let n = |s: f32| (s.max(0.0) * sr) as u32;
        let mut e = Self {
            stage: Stage::Delay,
            level: 0.0,
            counter: 0,
            delay: n(delay),
            attack: n(attack),
            hold: n(hold),
            decay_coef: exp_coef(decay, sr),
            sustain: sustain.clamp(0.0, 1.0),
            release_coef: exp_coef(release, sr),
        };
        if decay <= 0.0 { e.decay_coef = 0.0; }
        e
    }

    fn release(&mut self) {
        if self.stage != Stage::Done { self.stage = Stage::Release; }
    }

    #[inline]
    fn next(&mut self) -> f32 {
        loop {
            match self.stage {
                Stage::Delay => {
                    if self.counter < self.delay { self.counter += 1; return 0.0; }
                    self.stage = Stage::Attack;
                    self.counter = 0;
                }
                Stage::Attack => {
                    if self.counter < self.attack {
                        self.counter += 1;
                        self.level = self.counter as f32 / self.attack as f32;
                        return self.level;
                    }
                    self.level = 1.0;
                    self.stage = Stage::Hold;
                    self.counter = 0;
                }
                Stage::Hold => {
                    if self.counter < self.hold { self.counter += 1; return self.level; }
                    self.stage = Stage::Decay;
                }
                Stage::Decay => {
                    // Exponential approach: the distance above sustain shrinks.
                    let above = (self.level - self.sustain) * self.decay_coef;
                    self.level = self.sustain + above;
                    if above <= ENV_FLOOR { self.level = self.sustain; self.stage = Stage::Sustain; }
                    return self.level;
                }
                Stage::Sustain => {
                    if self.level <= ENV_FLOOR { self.stage = Stage::Done; return 0.0; }
                    return self.level;
                }
                Stage::Release => {
                    self.level *= self.release_coef;
                    if self.level <= ENV_FLOOR { self.stage = Stage::Done; return 0.0; }
                    return self.level;
                }
                Stage::Done => return 0.0,
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Voice {
    active: bool,
    region: usize,
    key: i32,
    pos: f64,
    step: f64,
    end: f64,
    loop_mode: LoopMode,
    loop_start: f64,
    loop_end: f64,
    gain_l: f32,
    gain_r: f32,
    env: Env,
    /// The key is up but the sustain pedal holds the voice.
    sustained: bool,
    /// Note-off (or pedal-up) already handled.
    released: bool,
    /// Linear choke fade from `off_by` / polyphony: samples left, total.
    choke_left: u32,
    choke_total: u32,
    group: i64,
    age: u64,
}

impl Voice {
    fn idle() -> Self {
        Self {
            active: false, region: 0, key: -1, pos: 0.0, step: 1.0, end: 0.0,
            loop_mode: LoopMode::NoLoop, loop_start: 0.0, loop_end: 0.0,
            gain_l: 0.0, gain_r: 0.0, env: Env::idle(), sustained: false, released: false,
            choke_left: 0, choke_total: 0, group: 0, age: 0,
        }
    }
}

/// Note-on bookkeeping per key, for release triggers.
#[derive(Clone, Copy, Default)]
struct KeyState {
    held: bool,
    vel: u8,
    on_time: u64,
    /// Released while the pedal was down: its release regions fire at pedal-up.
    release_pending: bool,
}

/// Player-side controls on top of what the instrument defines. Read at
/// voice start, so a change affects the next note.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Controls {
    /// Global fine tune.
    pub tune_cents: f32,
    /// Multiplies every region's release time.
    pub release_scale: f32,
    /// −1 = hard touch (v²), 0 = as played, +1 = soft touch (√v).
    pub vel_curve: f32,
}

impl Default for Controls {
    fn default() -> Self { Self { tune_cents: 0.0, release_scale: 1.0, vel_curve: 0.0 } }
}

pub struct Engine {
    pub controls: Controls,
    voices: Box<[Voice]>,
    keys: [KeyState; 128],
    cc: [f32; 128],
    sw: Option<i32>,
    sample_rate: f32,
    /// Samples rendered since construction — the clock for `rt_decay`.
    time: u64,
    next_age: u64,
    rng: Xorshift,
}

pub const DEFAULT_VOICES: usize = 128;
const PEDAL_CC: usize = 64;

impl Engine {
    pub fn new(sample_rate: f32, voices: usize) -> Self {
        Self {
            controls: Controls::default(),
            voices: vec![Voice::idle(); voices.max(1)].into_boxed_slice(),
            keys: [KeyState::default(); 128],
            cc: [0.0; 128],
            sw: None,
            sample_rate,
            time: 0,
            next_age: 0,
            rng: Xorshift::new(0x5F2_9A1),
        }
    }

    /// Silence everything and load the instrument's `<control>` CC defaults and
    /// default keyswitch. Call when a new instrument is published.
    pub fn reset(&mut self, inst: &Instrument) {
        for v in self.voices.iter_mut() { *v = Voice::idle(); }
        self.keys = [KeyState::default(); 128];
        self.cc = [0.0; 128];
        for &(n, v) in &inst.def.cc_defaults {
            if let Some(slot) = self.cc.get_mut(n as usize) { *slot = v; }
        }
        self.sw = inst.def.sw_default;
    }

    pub fn active_voices(&self) -> usize { self.voices.iter().filter(|v| v.active).count() }

    pub fn cc_value(&self, n: u8) -> f32 { self.cc.get(n as usize).copied().unwrap_or(0.0) }

    pub fn keyswitch(&self) -> Option<i32> { self.sw }

    pub fn pedal_is_down(&self) -> bool { self.pedal_down() }

    /// Per-key state for a keyboard display: 2 = key held, 1 = still sounding
    /// (pedal or release tail), 0 = silent. CC-triggered voices (pedal noise)
    /// are not shown on any key. Alloc-free.
    pub fn key_activity(&self, inst: &Instrument, out: &mut [u8; 128]) {
        out.fill(0);
        for v in self.voices.iter().filter(|v| v.active && (0..128).contains(&v.key)) {
            if inst.def.regions.get(v.region).is_some_and(|r| r.on_cc.is_none() && r.trigger != Trigger::Release) {
                out[v.key as usize] = 1;
            }
        }
        for (k, ks) in self.keys.iter().enumerate() {
            if ks.held { out[k] = 2; }
        }
    }

    fn pedal_down(&self) -> bool { self.cc[PEDAL_CC] >= 0.5 }

    fn region_allowed(&self, r: &Region) -> bool {
        if let (Some(want), Some(cur)) = (r.sw_last, self.sw) {
            if want != cur { return false; }
        }
        r.cc_conditions.iter().all(|c| c.contains(self.cc[c.cc as usize]))
    }

    fn mod_sum(&self, inst: &Instrument, r: &Region, target: ModTarget) -> f32 {
        r.mods.iter()
            .filter(|m| m.target == target)
            .map(|m| m.depth * inst.def.curves.eval(m.curve, self.cc[m.cc as usize]))
            .sum()
    }

    pub fn note_on(&mut self, inst: &Instrument, key: u8, velocity01: f32) {
        let key_i = key as i32;
        if let Some((lo, hi)) = inst.def.sw_range {
            if key_i >= lo && key_i <= hi { self.sw = Some(key_i); return; }
        }
        if key >= 128 { return; }
        let shaped = velocity01.clamp(0.0, 1.0).powf(2f32.powf(-self.controls.vel_curve.clamp(-1.0, 1.0)));
        let vel = (shaped * 127.0).round().clamp(1.0, 127.0) as u8;
        let others_held = self.keys.iter().enumerate().any(|(k, s)| s.held && k != key as usize);
        if let Some(ks) = self.keys.get_mut(key as usize) {
            *ks = KeyState { held: true, vel, on_time: self.time, release_pending: false };
        }
        let rand = self.rng.next_f32();
        for (i, r) in inst.def.regions.iter().enumerate() {
            let fits = match r.trigger {
                Trigger::Attack => true,
                Trigger::First => !others_held,
                Trigger::Legato => others_held,
                Trigger::Release => false,
            };
            if fits && r.on_cc.is_none() && r.key_matches(key_i) && r.vel_matches(vel)
                && r.rand_matches(rand) && self.region_allowed(r)
            {
                self.start_voice(inst, i, key_i, vel as f32 / 127.0, 0.0);
            }
        }
    }

    pub fn note_off(&mut self, inst: &Instrument, key: u8) {
        let Some(ks) = self.keys.get(key as usize).copied() else { return };
        if !ks.held { return; }
        let pedal = self.pedal_down();
        for v in self.voices.iter_mut().filter(|v| v.active && v.key == key as i32 && !v.released) {
            let Some(r) = inst.def.regions.get(v.region) else { continue };
            if r.trigger == Trigger::Release || r.on_cc.is_some() || v.loop_mode == LoopMode::OneShot { continue; }
            if pedal { v.sustained = true; } else { v.env.release(); v.released = true; }
        }
        self.keys[key as usize].held = false;
        if pedal {
            self.keys[key as usize].release_pending = true;
        } else {
            self.fire_release(inst, key, ks);
        }
    }

    fn fire_release(&mut self, inst: &Instrument, key: u8, ks: KeyState) {
        let held_s = (self.time.saturating_sub(ks.on_time)) as f32 / self.sample_rate;
        let rand = self.rng.next_f32();
        for (i, r) in inst.def.regions.iter().enumerate() {
            if r.trigger == Trigger::Release && r.on_cc.is_none() && r.key_matches(key as i32)
                && r.vel_matches(ks.vel) && r.rand_matches(rand) && self.region_allowed(r)
            {
                self.start_voice(inst, i, key as i32, ks.vel as f32 / 127.0, -r.rt_decay_db * held_s);
            }
        }
    }

    pub fn cc(&mut self, inst: &Instrument, n: u8, value01: f32) {
        let n_us = n as usize;
        if n_us >= 128 { return; }
        let prev = self.cc[n_us];
        let v = value01.clamp(0.0, 1.0);
        let was_down = self.pedal_down();
        self.cc[n_us] = v;
        if n_us == PEDAL_CC && was_down && !self.pedal_down() {
            for vc in self.voices.iter_mut().filter(|vc| vc.active && vc.sustained && !vc.released) {
                if !self.keys.get(vc.key as usize).is_some_and(|k| k.held) {
                    vc.env.release();
                    vc.released = true;
                    vc.sustained = false;
                }
            }
            for key in 0..128u8 {
                let ks = self.keys[key as usize];
                if ks.release_pending && !ks.held {
                    self.keys[key as usize].release_pending = false;
                    self.fire_release(inst, key, ks);
                }
            }
        }
        // CC-triggered regions start when the value ENTERS their range — a
        // continuous pedal sends dozens of values per press.
        for (i, r) in inst.def.regions.iter().enumerate() {
            if let Some(on) = r.on_cc {
                if on.cc == n && on.contains(v) && !on.contains(prev) && self.region_allowed(r) {
                    let rand = self.rng.next_f32();
                    if r.rand_matches(rand) {
                        self.start_voice(inst, i, r.pitch_keycenter, 1.0, 0.0);
                    }
                }
            }
        }
    }

    /// Release everything immediately-ish (CC 123 / transport stop).
    pub fn all_notes_off(&mut self) {
        for v in self.voices.iter_mut().filter(|v| v.active) {
            v.env.release();
            v.released = true;
            v.sustained = false;
        }
        for k in self.keys.iter_mut() { *k = KeyState::default(); }
    }

    fn choke(v: &mut Voice, seconds: f32, sr: f32) {
        let n = ((seconds * sr) as u32).max(16);
        if v.choke_left == 0 || n < v.choke_left {
            v.choke_left = n;
            v.choke_total = n;
        }
    }

    fn start_voice(&mut self, inst: &Instrument, region_idx: usize, key: i32, vel01: f32, extra_db: f32) {
        let Some(r) = inst.def.regions.get(region_idx) else { return };
        let Some(sample) = inst.samples.get(r.sample_index) else { return };
        if sample.frames == 0 { return; }
        let sr = self.sample_rate;

        // off_by: this region's group silences voices that name it.
        for v in self.voices.iter_mut().filter(|v| v.active) {
            if let Some(other) = inst.def.regions.get(v.region) {
                if other.off_by == Some(r.group) { Self::choke(v, other.off_time, sr); }
            }
        }
        // Polyphony caps: oldest non-choking voice goes first.
        let limits = [
            (r.polyphony, None),
            (r.note_polyphony, Some(key)),
        ];
        for (limit, key_filter) in limits {
            let Some(limit) = limit else { continue };
            loop {
                let mut count = 0u32;
                let mut oldest: Option<usize> = None;
                for (i, v) in self.voices.iter().enumerate() {
                    if !v.active || v.choke_left > 0 || v.group != r.group { continue; }
                    if key_filter.is_some_and(|k| k != v.key) { continue; }
                    count += 1;
                    if oldest.is_none_or(|o| v.age < self.voices[o].age) { oldest = Some(i); }
                }
                match oldest {
                    Some(i) if count >= limit => {
                        let t = inst.def.regions.get(self.voices[i].region).map_or(0.006, |x| x.off_time);
                        Self::choke(&mut self.voices[i], t, sr);
                    }
                    _ => break,
                }
            }
        }

        let amp_mods: f32 = r.mods.iter()
            .filter(|m| m.target == ModTarget::Amplitude)
            .map(|m| m.depth / 100.0 * inst.def.curves.eval(m.curve, self.cc[m.cc as usize]))
            .product();
        let volume_db = r.volume_db + self.mod_sum(inst, r, ModTarget::Volume) + extra_db;
        let veltrack = ((r.amp_veltrack_pct + self.mod_sum(inst, r, ModTarget::AmpVeltrack)) / 100.0).clamp(-1.0, 1.0);
        // sfizz's velocity curve: v², blended by |veltrack|; negative inverts.
        let curve = vel01 * vel01;
        let vel_gain = if veltrack >= 0.0 { 1.0 - veltrack * (1.0 - curve) } else { -veltrack * (1.0 - curve) };
        let gain = r.amplitude_pct / 100.0 * amp_mods * 10f32.powf(volume_db / 20.0) * vel_gain;
        let pan = ((r.pan + self.mod_sum(inst, r, ModTarget::Pan)) / 100.0).clamp(-1.0, 1.0);
        let theta = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
        let (gl, gr) = (std::f32::consts::SQRT_2 * theta.cos(), std::f32::consts::SQRT_2 * theta.sin());

        let semis = (key - r.pitch_keycenter) as f32 * r.pitch_keytrack / 100.0
            + r.transpose as f32
            + (r.tune_cents + self.mod_sum(inst, r, ModTarget::Tune) + self.controls.tune_cents) / 100.0;
        let step = 2f64.powf(semis as f64 / 12.0) * sample.sample_rate as f64 / sr as f64;
        let offset = (r.offset as f32 + self.mod_sum(inst, r, ModTarget::Offset)).max(0.0) as f64;
        let end = r.end.map_or(sample.frames, |e| (e as usize + 1).min(sample.frames)) as f64;
        if offset >= end { return; }

        let env = Env::start(
            r.ampeg_delay,
            r.ampeg_attack + self.mod_sum(inst, r, ModTarget::AmpegAttack),
            r.ampeg_hold,
            r.ampeg_decay + self.mod_sum(inst, r, ModTarget::AmpegDecay),
            (r.ampeg_sustain_pct + self.mod_sum(inst, r, ModTarget::AmpegSustain)) / 100.0,
            (r.ampeg_release + self.mod_sum(inst, r, ModTarget::AmpegRelease)) * self.controls.release_scale.max(0.01),
            sr,
        );
        let loop_mode = r.effective_loop_mode();
        let (ls, le) = match (r.loop_start, r.loop_end) {
            (Some(a), Some(b)) if b > a => (a as f64, (b as f64 + 1.0).min(end)),
            _ => (0.0, end),
        };

        self.next_age = self.next_age.wrapping_add(1);
        let voice = Voice {
            active: true,
            region: region_idx,
            key,
            pos: offset,
            step,
            end,
            loop_mode,
            loop_start: ls,
            loop_end: le,
            gain_l: gain * gl,
            gain_r: gain * gr,
            env,
            sustained: false,
            released: r.trigger == Trigger::Release || r.on_cc.is_some(),
            choke_left: 0,
            choke_total: 0,
            group: r.group,
            age: self.next_age,
        };
        let slot = self.free_slot();
        self.voices[slot] = voice;
    }

    /// An idle slot, else the oldest released voice, else the oldest.
    fn free_slot(&self) -> usize {
        if let Some(i) = self.voices.iter().position(|v| !v.active) { return i; }
        let oldest = |released: bool| {
            self.voices.iter().enumerate()
                .filter(|(_, v)| v.released == released)
                .min_by_key(|(_, v)| v.age)
                .map(|(i, _)| i)
        };
        oldest(true).or_else(|| oldest(false)).unwrap_or(0)
    }

    /// Add the instrument's output into `out_l` / `out_r` (they are not
    /// cleared first, so several engines can share a buffer).
    pub fn render(&mut self, inst: &Instrument, out_l: &mut [f32], out_r: &mut [f32]) {
        let frames = out_l.len().min(out_r.len());
        for v in self.voices.iter_mut().filter(|v| v.active) {
            let Some(r) = inst.def.regions.get(v.region) else { v.active = false; continue };
            let Some(s) = inst.samples.get(r.sample_index) else { v.active = false; continue };
            for i in 0..frames {
                let mut e = v.env.next();
                if v.env.stage == Stage::Done { v.active = false; break; }
                if v.choke_left > 0 {
                    e *= v.choke_left as f32 / v.choke_total.max(1) as f32;
                    v.choke_left -= 1;
                    if v.choke_left == 0 { v.active = false; break; }
                }
                let ip = v.pos.floor();
                let t = (v.pos - ip) as f32;
                let i0 = ip as isize;
                let (am, bm) = s.frame(i0 - 1);
                let (a0, b0) = s.frame(i0);
                let (a1, b1) = s.frame(i0 + 1);
                let (a2, b2) = s.frame(i0 + 2);
                let l = hermite(am, a0, a1, a2, t);
                let rr = hermite(bm, b0, b1, b2, t);
                if let (Some(ol), Some(or)) = (out_l.get_mut(i), out_r.get_mut(i)) {
                    *ol += l * v.gain_l * e;
                    *or += rr * v.gain_r * e;
                }
                v.pos += v.step;
                let looping = match v.loop_mode {
                    LoopMode::Continuous => true,
                    LoopMode::Sustain => !v.released,
                    _ => false,
                };
                if looping && v.pos >= v.loop_end {
                    v.pos -= (v.loop_end - v.loop_start).max(1.0);
                } else if v.pos >= v.end {
                    v.active = false;
                    break;
                }
            }
        }
        self.time = self.time.wrapping_add(frames as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sfz::{parse::parse_sfz, region::compile};

    /// An instrument whose samples are synthetic so tests need no files:
    /// every sample is a 1 s 440 Hz-ish ramp-free constant (0.5).
    fn inst(text: &str) -> Instrument {
        let mut read = |_: &str| None;
        let def = compile(&parse_sfz(text, &mut read).expect("parse"));
        let samples = def.sample_paths.iter()
            .map(|_| Arc::new(SampleBuf { frames: 48_000, channels: 1, sample_rate: 48_000.0, data: vec![16384i16; 48_000].into_boxed_slice() }))
            .collect();
        Instrument { def, samples, missing: vec![], name: "t".into() }
    }

    fn render(e: &mut Engine, i: &Instrument, n: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r) = (vec![0.0; n], vec![0.0; n]);
        e.render(i, &mut l, &mut r);
        (l, r)
    }

    #[test]
    fn velocity_layers_pick_one_region() {
        let i = inst("<region> sample=soft.wav lovel=1 hivel=64 <region> sample=hard.wav lovel=65 hivel=127");
        let mut e = Engine::new(48_000.0, 16);
        e.reset(&i);
        e.note_on(&i, 60, 0.3);
        assert_eq!(e.active_voices(), 1);
        assert_eq!(e.voices.iter().find(|v| v.active).map(|v| v.region), Some(0));
        e.note_on(&i, 62, 0.9);
        assert_eq!(e.voices.iter().filter(|v| v.active).map(|v| v.region).max(), Some(1));
    }

    #[test]
    fn full_velocity_at_unity_gain_center_pan() {
        let i = inst("<region> sample=a.wav");
        let mut e = Engine::new(48_000.0, 4);
        e.reset(&i);
        e.note_on(&i, 60, 1.0);
        let (l, r) = render(&mut e, &i, 100);
        assert!((l[50] - 0.5).abs() < 1e-3, "l={}", l[50]);
        assert!((r[50] - 0.5).abs() < 1e-3);
    }

    #[test]
    fn sustain_pedal_holds_then_releases_and_fires_release_triggers_late() {
        let i = inst("<region> sample=note.wav ampeg_release=0.01 <region> sample=rel.wav trigger=release");
        let mut e = Engine::new(48_000.0, 8);
        e.reset(&i);
        e.cc(&i, 64, 1.0);
        e.note_on(&i, 60, 1.0);
        e.note_off(&i, 60);
        let _ = render(&mut e, &i, 4800);
        assert_eq!(e.active_voices(), 1, "pedal holds the note, release sample waits");
        e.cc(&i, 64, 0.0);
        assert_eq!(e.active_voices(), 2, "pedal up fires the release region");
        let _ = render(&mut e, &i, 4800);
        assert!(e.voices.iter().filter(|v| v.active).all(|v| v.region == 1), "the held note died after release");
    }

    #[test]
    fn note_polyphony_one_chokes_the_previous_strike() {
        let i = inst("<region> sample=a.wav note_polyphony=1 off_time=0.01");
        let mut e = Engine::new(48_000.0, 8);
        e.reset(&i);
        e.note_on(&i, 60, 1.0);
        e.note_on(&i, 60, 1.0);
        let _ = render(&mut e, &i, 1000);
        assert_eq!(e.active_voices(), 1);
    }

    #[test]
    fn keyswitch_selects_articulation_and_does_not_sound() {
        let i = inst("<global> sw_lokey=12 sw_hikey=13 sw_default=12 <master> sw_last=12 <region> sample=a.wav <master> sw_last=13 <region> sample=b.wav");
        let mut e = Engine::new(48_000.0, 8);
        e.reset(&i);
        e.note_on(&i, 60, 1.0);
        assert_eq!(e.voices.iter().find(|v| v.active).map(|v| v.region), Some(0));
        e.note_on(&i, 13, 1.0);
        assert_eq!(e.active_voices(), 1, "keyswitch is silent");
        e.note_on(&i, 62, 1.0);
        assert!(e.voices.iter().any(|v| v.active && v.region == 1));
    }

    #[test]
    fn cc_triggered_region_fires_once_on_entering_range() {
        let i = inst("<region> sample=pedal.wav lokey=-1 hikey=-1 on_locc64=127 on_hicc64=127");
        let mut e = Engine::new(48_000.0, 8);
        e.reset(&i);
        e.cc(&i, 64, 1.0);
        e.cc(&i, 64, 1.0);
        assert_eq!(e.active_voices(), 1);
        e.note_on(&i, 60, 1.0);
        assert_eq!(e.active_voices(), 1, "key -1 never matches a note");
    }

    #[test]
    fn amplitude_oncc_scales_and_cc_condition_gates() {
        let i = inst("<control> set_cc7=127 set_hdcc20=0.5 <region> sample=a.wav amplitude_oncc20=200 locc20=1");
        let mut e = Engine::new(48_000.0, 4);
        e.reset(&i);
        e.note_on(&i, 60, 1.0);
        let (l, _) = render(&mut e, &i, 100);
        assert!((l[50] - 0.5).abs() < 1e-2, "200% × 0.5 = unity, got {}", l[50]);
        e.all_notes_off();
        e.cc(&i, 20, 0.0);
        e.note_on(&i, 60, 1.0);
        assert!(e.voices.iter().filter(|v| v.active && !v.released).count() == 0, "locc20=1 gates it off");
    }

    #[test]
    fn controls_tune_and_soft_velocity_curve() {
        let i = inst("<region> sample=a.wav pitch_keycenter=60");
        let mut e = Engine::new(48_000.0, 4);
        e.reset(&i);
        e.controls.tune_cents = 1200.0;
        e.note_on(&i, 60, 1.0);
        let v = e.voices.iter().find(|v| v.active).copied().expect("voice");
        assert!((v.step - 2.0).abs() < 1e-6, "tune +1200 ct = octave");
        e.all_notes_off();
        let (soft, hard) = {
            let mut g = |curve: f32| {
                let mut e = Engine::new(48_000.0, 4);
                e.reset(&i);
                e.controls.vel_curve = curve;
                e.note_on(&i, 60, 0.25);
                e.voices.iter().find(|v| v.active).map_or(0.0, |v| v.gain_l)
            };
            (g(1.0), g(-1.0))
        };
        assert!(soft > hard * 4.0, "soft touch is louder at low velocity: {soft} vs {hard}");
    }

    #[test]
    fn key_activity_shows_held_and_pedal_sustained_keys() {
        let i = inst("<region> sample=a.wav");
        let mut e = Engine::new(48_000.0, 8);
        e.reset(&i);
        e.cc(&i, 64, 1.0);
        e.note_on(&i, 60, 1.0);
        e.note_on(&i, 64, 1.0);
        e.note_off(&i, 64);
        let mut a = [0u8; 128];
        e.key_activity(&i, &mut a);
        assert_eq!((a[60], a[64], a[62]), (2, 1, 0));
    }

    #[test]
    fn pitch_follows_key_distance() {
        let i = inst("<region> sample=a.wav pitch_keycenter=60");
        let mut e = Engine::new(48_000.0, 4);
        e.reset(&i);
        e.note_on(&i, 72, 1.0);
        let v = e.voices.iter().find(|v| v.active).copied().expect("voice");
        assert!((v.step - 2.0).abs() < 1e-9);
    }
}
