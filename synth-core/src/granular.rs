//! Real-time granular cloud — chop the incoming audio into hundreds of short
//! windowed fragments and reassemble them into a living texture.
//!
//! This is our answer to Emergence: a live granulator that runs on the input
//! rather than on a loaded file. The input streams into a circular capture
//! buffer, a scheduler keeps spawning grains that read from somewhere behind the
//! write head, and each grain gets its own pitch, pan, direction and window.
//!
//! Two controls carry most of the character:
//!
//! - **Freeze** stops the capture. The cloud then chews on the last few seconds
//!   forever — one sung note becomes an endless pad. This is the reason the
//!   plugin exists; everything else is shaping.
//! - **Feedback** writes the cloud's own output back into the buffer, so grains
//!   granulate grains. A `DcBlocker` sits in that path (lesson 12: DC in a
//!   feedback loop accumulates and eventually drowns the signal).
//!
//! **RT-safe:** the capture buffer and the grain pool are allocated in
//! [`GranularCloud::new`]; [`process`](GranularCloud::process) never allocates,
//! locks, or panics. Randomness is a deterministic xorshift so tests reproduce.

use crate::dsp_blocks::{DcBlocker, Xorshift};

/// Grain pool size. At 200 grains/s with 500 ms grains you'd want 100 voices;
/// this caps CPU and the scheduler simply skips a spawn when the pool is full.
pub const MAX_GRAINS: usize = 96;

/// Capture buffer length in seconds.
pub const BUFFER_SECONDS: f32 = 6.0;

/// Grain window shapes.
pub const SHAPE_HANN: u32 = 0;
pub const SHAPE_TUKEY: u32 = 1;
pub const SHAPE_PERC: u32 = 2;

#[derive(Clone, Copy)]
pub struct GrainParams {
    /// Grains per second.
    pub density: f32,
    /// Grain length in ms.
    pub size_ms: f32,
    /// Random spread of each grain's start position (0..1 of the buffer).
    pub spray: f32,
    /// How far behind the write head grains start (0..1 of the buffer).
    pub position: f32,
    /// Transpose in semitones.
    pub pitch_semi: f32,
    /// Random per-grain pitch spread in semitones (±).
    pub jitter_semi: f32,
    /// Stereo spread of the per-grain pan (0 = mono, 1 = hard L/R).
    pub spread: f32,
    /// Probability (0..1) that a grain plays backwards.
    pub reverse: f32,
    /// Stop capturing — the cloud loops the buffer contents forever.
    pub freeze: bool,
    /// Output fed back into the capture buffer (0..0.95).
    pub feedback: f32,
    pub shape: u32,
    pub mix: f32,
    pub output_lin: f32,
    pub bypassed: bool,
}

impl Default for GrainParams {
    fn default() -> Self {
        Self {
            density: 20.0,
            size_ms: 80.0,
            spray: 0.2,
            position: 0.05,
            pitch_semi: 0.0,
            jitter_semi: 0.0,
            spread: 0.5,
            reverse: 0.0,
            freeze: false,
            feedback: 0.0,
            shape: SHAPE_HANN,
            mix: 1.0,
            output_lin: 1.0,
            bypassed: false,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Grain {
    active: bool,
    /// Fractional read position in the capture buffer.
    pos: f32,
    /// Per-sample read increment (negative = reverse).
    step: f32,
    /// Samples elapsed.
    age: f32,
    /// Total length in samples.
    len: f32,
    gain_l: f32,
    gain_r: f32,
    shape: u32,
}

pub struct GranularCloud {
    sr: f32,
    cap: usize,
    buf_l: Box<[f32]>,
    buf_r: Box<[f32]>,
    write: usize,
    grains: Box<[Grain]>,
    /// Samples until the next spawn (fractional).
    to_spawn: f32,
    rng: Xorshift,
    /// Last output, for the feedback path.
    fb_l: f32,
    fb_r: f32,
    dc_l: DcBlocker,
    dc_r: DcBlocker,
    /// Grains currently sounding — worth showing in a GUI.
    live_grains: usize,
    /// Samples written into the ring since construction. A Freeze that arrives
    /// before anything was captured must not lock the cloud onto silence.
    captured: usize,
}

impl GranularCloud {
    pub fn new(sr: f32) -> Self {
        let cap = ((sr * BUFFER_SECONDS) as usize).max(1024);
        Self {
            sr,
            cap,
            buf_l: vec![0.0; cap].into_boxed_slice(),
            buf_r: vec![0.0; cap].into_boxed_slice(),
            write: 0,
            grains: vec![Grain::default(); MAX_GRAINS].into_boxed_slice(),
            to_spawn: 0.0,
            rng: Xorshift::new(0x1234_5678),
            fb_l: 0.0,
            fb_r: 0.0,
            dc_l: DcBlocker::default(),
            dc_r: DcBlocker::default(),
            live_grains: 0,
            captured: 0,
        }
    }

    pub fn reset(&mut self) {
        self.buf_l.fill(0.0);
        self.buf_r.fill(0.0);
        self.write = 0;
        for g in self.grains.iter_mut() {
            *g = Grain::default();
        }
        self.to_spawn = 0.0;
        self.fb_l = 0.0;
        self.fb_r = 0.0;
        self.live_grains = 0;
        self.captured = 0;
    }

    #[inline]
    pub fn live_grains(&self) -> usize {
        self.live_grains
    }

    /// Buffer fill position 0..1 — a GUI can draw the write head with it.
    #[inline]
    pub fn write_phase(&self) -> f32 {
        self.write as f32 / self.cap as f32
    }

    /// Hermite (Catmull-Rom) interpolated read — linear interpolation on a grain
    /// read at a transposed rate is audibly grainy in the wrong way (a dull
    /// high-shelf that changes with pitch).
    #[inline]
    fn read_at(buf: &[f32], cap: usize, pos: f32) -> f32 {
        let i = pos.floor();
        let frac = pos - i;
        let i0 = ((i as isize).rem_euclid(cap as isize)) as usize;
        let im1 = if i0 == 0 { cap - 1 } else { i0 - 1 };
        let i1 = if i0 + 1 >= cap { 0 } else { i0 + 1 };
        let i2 = if i1 + 1 >= cap { 0 } else { i1 + 1 };
        let (xm1, x0, x1, x2) = (buf[im1], buf[i0], buf[i1], buf[i2]);
        let c = (x1 - xm1) * 0.5;
        let v = x0 - x1;
        let w = c + v;
        let a = w + v + (x2 - x0) * 0.5;
        let b = w + a;
        ((a * frac - b) * frac + c) * frac + x0
    }

    /// Window amplitude at `t` ∈ [0, 1].
    #[inline]
    fn window(shape: u32, t: f32) -> f32 {
        match shape {
            SHAPE_TUKEY => {
                // Flat middle with short raised-cosine edges — keeps more of the
                // source's own envelope, reads as "sampler" rather than "cloud".
                const EDGE: f32 = 0.15;
                if t < EDGE {
                    0.5 - 0.5 * (core::f32::consts::PI * t / EDGE).cos()
                } else if t > 1.0 - EDGE {
                    0.5 - 0.5 * (core::f32::consts::PI * (1.0 - t) / EDGE).cos()
                } else {
                    1.0
                }
            }
            SHAPE_PERC => {
                // Instant attack, exponential decay — percussive, pointillist.
                (-5.0 * t).exp() * (1.0 - t).max(0.0)
            }
            // Hann — the smooth default, no clicks at any grain length.
            _ => 0.5 - 0.5 * (core::f32::consts::TAU * t).cos(),
        }
    }

    fn spawn(&mut self, p: &GrainParams) {
        let Some(slot) = self.grains.iter().position(|g| !g.active) else {
            // Pool exhausted — skipping a spawn is the honest behaviour; stealing
            // a sounding grain would click.
            return;
        };
        let len = (p.size_ms.clamp(1.0, 2000.0) * 0.001 * self.sr).max(8.0);
        let semis = p.pitch_semi + self.rng.next_bipolar() * p.jitter_semi;
        let ratio = (semis / 12.0).exp2().clamp(0.03, 32.0);
        let reverse = p.reverse > 0.0 && self.rng.next_f32() < p.reverse;
        // Start behind the write head: `position` back, plus a random spray.
        //
        // The guard has to cover the whole span the grain will traverse, not a
        // token fraction of the ring: a reverse grain begins `len·ratio` ahead of
        // its start point, and a forward grain pitched up by `ratio` closes on
        // the write head at `(ratio − 1)` samples per sample. A flat 0.2 % guard
        // let an 80 ms reverse grain — or any +12 st grain — read straight across
        // the write pointer into 6-second-old audio, splicing a hard
        // discontinuity mid-grain: exactly the click this guard exists to stop.
        let span = len * ratio.max(1.0) + 64.0;
        let min_back = (span / self.cap as f32).min(0.9);
        let back_frac = (p.position.clamp(0.0, 1.0) * 0.9
            + self.rng.next_f32() * p.spray.clamp(0.0, 1.0) * 0.9)
            .clamp(min_back, 0.98);
        let back = back_frac * self.cap as f32;
        let start = self.write as f32 - back;
        // Equal-power pan from a random position scaled by Spread.
        let pan = self.rng.next_bipolar() * p.spread.clamp(0.0, 1.0);
        let theta = (pan * 0.5 + 0.5) * core::f32::consts::FRAC_PI_2;

        self.grains[slot] = Grain {
            active: true,
            pos: if reverse { start + len * ratio } else { start },
            step: if reverse { -ratio } else { ratio },
            age: 0.0,
            len,
            gain_l: theta.cos(),
            gain_r: theta.sin(),
            shape: p.shape,
        };
    }

    /// Process one block. `write_r` may be empty for a mono track.
    pub fn process(
        &mut self,
        read_l: &[f32],
        read_r: &[f32],
        write_l: &mut [f32],
        write_r: &mut [f32],
        p: &GrainParams,
    ) {
        let n = read_l.len().min(write_l.len());
        if p.bypassed {
            write_l[..n].copy_from_slice(&read_l[..n]);
            if !write_r.is_empty() {
                let m = n.min(write_r.len()).min(read_r.len());
                write_r[..m].copy_from_slice(&read_r[..m]);
            }
            return;
        }

        // Freezing an empty ring would grind silence forever — which is what a
        // fresh instance recalling the "Freeze Pad" preset used to do. Until at
        // least one grain's worth of audio exists, Freeze is a no-op.
        let grain_samples = p.size_ms.clamp(1.0, 2000.0) * 0.001 * self.sr;
        let freeze = p.freeze && self.captured as f32 >= grain_samples.max(self.sr * 0.05);

        let density = p.density.clamp(0.05, 400.0);
        let interval = self.sr / density;
        let feedback = p.feedback.clamp(0.0, 0.95);
        let mix = p.mix.clamp(0.0, 1.0);
        // Overlap-based gain compensation: with `density × size` grains sounding
        // at once the sum grows, so normalise by √overlap (power, not amplitude —
        // the grains are decorrelated).
        let overlap = (density * p.size_ms.clamp(1.0, 2000.0) * 0.001).max(1.0);
        let norm = 1.0 / overlap.sqrt();

        for i in 0..n {
            let dry_l = read_l[i];
            let dry_r = if i < read_r.len() { read_r[i] } else { dry_l };

            // ---- Capture (unless frozen) --------------------------------
            if !freeze {
                self.buf_l[self.write] = dry_l + self.dc_l.process(self.fb_l) * feedback;
                self.buf_r[self.write] = dry_r + self.dc_r.process(self.fb_r) * feedback;
                self.write += 1;
                if self.write >= self.cap {
                    self.write = 0;
                }
                self.captured = self.captured.saturating_add(1);
            }

            // ---- Schedule ------------------------------------------------
            self.to_spawn -= 1.0;
            let mut guard = 0;
            while self.to_spawn <= 0.0 && guard < 8 {
                self.spawn(p);
                self.to_spawn += interval;
                guard += 1;
            }
            if self.to_spawn < 0.0 {
                // Density so high the interval is under a sample — clamp rather
                // than spin.
                self.to_spawn = 0.0;
            }

            // ---- Render --------------------------------------------------
            let mut wet_l = 0.0f32;
            let mut wet_r = 0.0f32;
            let mut live = 0usize;
            for g in self.grains.iter_mut() {
                if !g.active {
                    continue;
                }
                let t = g.age / g.len;
                if t >= 1.0 {
                    g.active = false;
                    continue;
                }
                let w = Self::window(g.shape, t);
                let s_l = Self::read_at(&self.buf_l, self.cap, g.pos);
                let s_r = Self::read_at(&self.buf_r, self.cap, g.pos);
                // Pan is applied to the mono sum of the grain so the placement is
                // real (panning an already-stereo grain does nothing audible).
                let mono = (s_l + s_r) * 0.5;
                wet_l += mono * w * g.gain_l;
                wet_r += mono * w * g.gain_r;
                g.pos += g.step;
                g.age += 1.0;
                live += 1;
            }
            self.live_grains = live;

            wet_l *= norm;
            wet_r *= norm;
            self.fb_l = wet_l;
            self.fb_r = wet_r;

            write_l[i] = (dry_l * (1.0 - mix) + wet_l * mix) * p.output_lin;
            if !write_r.is_empty() && i < write_r.len() {
                write_r[i] = (dry_r * (1.0 - mix) + wet_r * mix) * p.output_lin;
            }
        }
    }
}
