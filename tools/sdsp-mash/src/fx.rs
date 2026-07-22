//! One-shot performance effects placed on the timeline — the "simple + wow"
//! moves that make a mashup feel like a DJ set rather than two stems glued
//! together. Each is a pure function over a stereo buffer + a sample region.
//!
//! - [`tape_stop`] — resampling ramp to zero (the "power-down" pitch drop).
//! - [`beat_repeat`] — accelerating downbeat stutter (1/2 → 1/4 → 1/8).
//! - [`echo_out`] — feed the tail into a feedback delay and let it ring.
//! - [`kick_pump`] — grid-locked sidechain pump on a melodic bus.
//! - [`riser`] — white-noise build: opening high-pass + volume ramp.

use superduper_synth_core::dsp_blocks::{Biquad, DelayLine};

use crate::sweep::{apply_sweep, SweepParams};

/// A timeline effect kind + its resolved (sample-domain) placement/params.
#[derive(Debug, Clone, Copy)]
pub enum FxKind {
    TapeStop,
    BeatRepeat,
    EchoOut,
    KickPump,
    Riser,
    /// Opening lowpass across a region (used by the `filter_sweep` transition).
    FilterSweep,
    /// Sub sine gliding down + a noise impact — the chest-hit on a drop.
    SubDrop,
    /// Rising high-pass over a region — the "filter breathing" build.
    HpSweep,
    /// Reverse-and-accelerate through the region (vinyl backspin scratch).
    Backspin,
}

#[derive(Debug, Clone, Copy)]
pub struct ResolvedFx {
    pub kind: FxKind,
    pub start: usize,
    pub len: usize,
    pub from_hz: f64,
    pub to_hz: f64,
    pub feedback: f32,
    pub delay_samples: usize,
    pub depth_db: f32,
    pub release_ms: f32,
    pub peak: f32,
}

/// Apply a batch of timeline effects to the pre-master mix. Rewriting/shaping
/// fx (tape_stop, beat_repeat, filters, pump) run first, additive ones (riser,
/// sub_drop, echo_out) second, each pass in start order — an overlapping
/// rewrite would otherwise erase a riser's swell (a thin, weightless
/// transition instead of a hit).
pub fn apply_all(l: &mut [f32], r: &mut [f32], sr: u32, bpm: f64, fx: &[ResolvedFx]) {
    let additive =
        |k: FxKind| matches!(k, FxKind::Riser | FxKind::SubDrop | FxKind::EchoOut);
    let mut ordered = fx.to_vec();
    ordered.sort_by_key(|f| (additive(f.kind), f.start));
    for f in &ordered {
        match f.kind {
            FxKind::TapeStop => tape_stop(l, r, f.start, f.len),
            FxKind::BeatRepeat => beat_repeat(l, r, f.start, f.len),
            FxKind::EchoOut => echo_out(l, r, f.start, f.len, f.delay_samples, f.feedback),
            FxKind::KickPump => {
                kick_pump(l, r, sr, bpm, f.start, f.len, f.depth_db, f.release_ms)
            }
            FxKind::Riser => {
                riser(l, r, sr, f.start, f.len, f.from_hz, f.to_hz, f.peak)
            }
            FxKind::FilterSweep => filter_sweep(l, r, sr, f.start, f.len, f.from_hz, f.to_hz),
            FxKind::SubDrop => sub_drop(l, r, sr, f.start, f.len, f.from_hz, f.to_hz, f.peak),
            FxKind::HpSweep => hp_sweep(l, r, sr, f.start, f.len, f.from_hz, f.to_hz),
            FxKind::Backspin => backspin(l, r, f.start, f.len),
        }
    }
}

/// Sub-drop: a sine gliding `from_hz` → `to_hz` (e.g. 55 → 45 Hz) over `len`
/// with a fast-attack / long-decay envelope, plus a short broadband noise
/// impact at the top — the Fred-again "chest-hit" on a drop. Additive.
pub fn sub_drop(
    l: &mut [f32],
    r: &mut [f32],
    sr: u32,
    start: usize,
    len: usize,
    from_hz: f64,
    to_hz: f64,
    peak: f32,
) {
    let n = l.len().min(r.len());
    if start >= n || len == 0 {
        return;
    }
    let end = (start + len).min(n);
    let span = (end - start) as f32;
    let mut rng = Rng::new(0x5b_d0_9a_c3 ^ start as u64);
    let impact = ((sr as f32) * 0.03) as usize; // 30 ms noise transient
    let mut hp = Biquad::default();
    hp.set_hpf(sr as f32, 800.0, 0.7); // brighten the impact so it "clicks"
    let mut phase = 0.0f32;
    let two_pi = 2.0 * std::f32::consts::PI;
    for k in 0..(end - start) {
        let t = k as f32 / span;
        // Exponential glide down (even in log-frequency).
        let freq = (from_hz as f32) * ((to_hz as f32) / from_hz as f32).powf(t);
        phase += two_pi * freq / sr as f32;
        // Fast attack (~5 ms), then decay to zero across the region.
        let atk = (k as f32 / (0.005 * sr as f32)).min(1.0);
        let sub = phase.sin() * atk * (1.0 - t).powf(1.4);
        let mut s = sub;
        if k < impact {
            let ie = 1.0 - k as f32 / impact as f32;
            s += hp.process(rng.bipolar()) * ie * 0.5;
        }
        let v = s * peak;
        l[start + k] += v;
        r[start + k] += v;
    }
}

/// Rising high-pass across `[start, start+len)` — thins the mix from the bottom
/// up (tension), then the region ends and full low end snaps back. The
/// "filter breathing" mini-build. Cutoff climbs `from_hz` → `to_hz`.
pub fn hp_sweep(l: &mut [f32], r: &mut [f32], sr: u32, start: usize, len: usize, from_hz: f64, to_hz: f64) {
    let n = l.len().min(r.len());
    if start >= n || len == 0 {
        return;
    }
    let end = (start + len).min(n);
    let span = (end - start) as f32;
    let mut fl = Biquad::default();
    let mut fr = Biquad::default();
    const BLK: usize = 64;
    for k in 0..(end - start) {
        if k % BLK == 0 {
            let t = k as f32 / span;
            let cutoff = ((from_hz as f32) * ((to_hz as f32) / from_hz as f32).powf(t))
                .clamp(20.0, sr as f32 * 0.45);
            fl.set_hpf(sr as f32, cutoff, 0.707);
            fr.set_hpf(sr as f32, cutoff, 0.707);
        }
        l[start + k] = fl.process(l[start + k]);
        r[start + k] = fr.process(r[start + k]);
    }
}

/// Opening lowpass across `[start, start+len)` — a filter-sweep transition on
/// the music (as opposed to the noise [`riser`]). Reuses the sweep engine.
pub fn filter_sweep(l: &mut [f32], r: &mut [f32], sr: u32, start: usize, len: usize, from_hz: f64, to_hz: f64) {
    let n = l.len().min(r.len());
    if start >= n || len == 0 {
        return;
    }
    let end = (start + len).min(n);
    let p = SweepParams {
        len_samples: end - start,
        from_hz: from_hz as f32,
        to_hz: to_hz as f32,
    };
    apply_sweep(&mut l[start..end], &mut r[start..end], sr as f32, &p);
}

/// Deterministic xorshift PRNG so noise-based effects (riser) are testable.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    /// Uniform in [-1, 1).
    #[inline]
    fn bipolar(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        ((x >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

#[inline]
fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

/// Tape-stop: over `[start, start+len)` the region's own audio is replayed at
/// a playback rate ramping linearly 1.0 → 0.0, dropping pitch + speed to a
/// halt. Only the first ~half of the region's source is consumed (∫rate).
pub fn tape_stop(l: &mut [f32], r: &mut [f32], start: usize, len: usize) {
    let n = l.len().min(r.len());
    if start >= n || len == 0 {
        return;
    }
    let end = (start + len).min(n);
    let span = end - start;
    let src_l: Vec<f32> = l[start..end].to_vec();
    let src_r: Vec<f32> = r[start..end].to_vec();

    let read = |src: &[f32], pos: f32| -> f32 {
        let i = pos.floor() as usize;
        if i + 1 < src.len() {
            let f = pos - i as f32;
            src[i] * (1.0 - f) + src[i + 1] * f
        } else if i < src.len() {
            src[i]
        } else {
            *src.last().unwrap_or(&0.0)
        }
    };

    let mut phase = 0.0f32;
    for k in 0..span {
        let rate = 1.0 - k as f32 / span as f32; // 1 → 0
        l[start + k] = read(&src_l, phase);
        r[start + k] = read(&src_r, phase);
        phase += rate;
    }
}

/// Backspin: over `[start, start+len)` the region's own audio is replayed
/// *backward*, starting from the tail, at a reverse-rate ramping 0.3 → 3.0 —
/// the classic vinyl-backspin scratch, a rising pitched "whoosh" that spins
/// up as it nears the cut. Use as an alternative to `tape_stop` on a lead-out
/// when the transition should feel yanked-away rather than powered-down.
pub fn backspin(l: &mut [f32], r: &mut [f32], start: usize, len: usize) {
    let n = l.len().min(r.len());
    if start >= n || len == 0 {
        return;
    }
    let end = (start + len).min(n);
    let span = end - start;
    let src_l: Vec<f32> = l[start..end].to_vec();
    let src_r: Vec<f32> = r[start..end].to_vec();

    let read = |src: &[f32], pos: f32| -> f32 {
        let i = pos.floor() as usize;
        if i + 1 < src.len() {
            let f = pos - i as f32;
            src[i] * (1.0 - f) + src[i + 1] * f
        } else if i < src.len() {
            src[i]
        } else {
            *src.last().unwrap_or(&0.0)
        }
    };

    let mut phase = (span.max(1) - 1) as f32;
    for k in 0..span {
        let t = k as f32 / span as f32;
        let rate = 0.3 + t * 2.7; // 0.3× → 3.0× reverse speed
        l[start + k] = read(&src_l, phase);
        r[start + k] = read(&src_r, phase);
        phase = (phase - rate).max(0.0);
    }
}

/// Beat-repeat: overwrite `[start, start+bar_len)` with an accelerating
/// stutter of the bar's downbeat — halves [1/2, 1/4, 1/8, 1/8] of the bar,
/// each replayed from the bar's first sample.
pub fn beat_repeat(l: &mut [f32], r: &mut [f32], start: usize, bar_len: usize) {
    let n = l.len().min(r.len());
    if start >= n || bar_len == 0 {
        return;
    }
    let end = (start + bar_len).min(n);
    let src_l: Vec<f32> = l[start..end].to_vec();
    let src_r: Vec<f32> = r[start..end].to_vec();
    let avail = src_l.len();

    // Slice lengths (samples) that tile one bar: 1/2, 1/4, 1/8, 1/8.
    let half = bar_len / 2;
    let quarter = bar_len / 4;
    let eighth = bar_len / 8;
    let schedule = [half, quarter, eighth, eighth];

    let mut cursor = 0usize;
    for &slice in &schedule {
        if slice == 0 {
            continue;
        }
        for k in 0..slice {
            if cursor >= (end - start) {
                break;
            }
            // Loop the first `slice` samples of the bar's source.
            let s = k % slice;
            if s < avail {
                l[start + cursor] = src_l[s];
                r[start + cursor] = src_r[s];
            }
            cursor += 1;
        }
    }
}

/// Echo-out: over `[start, start+len)` a feedback delay (primed with the
/// preceding `delay_samples`) rings decaying echoes on top of the mix. The dry
/// is kept — the echoes are *added*, so the effect is a flavour tail at a
/// transition, not a mute. Bounded by `len` so it never touches the rest of
/// the track.
pub fn echo_out(
    l: &mut [f32],
    r: &mut [f32],
    start: usize,
    len: usize,
    delay_samples: usize,
    feedback: f32,
) {
    let n = l.len().min(r.len());
    if start >= n || delay_samples == 0 || len == 0 {
        return;
    }
    let end = (start + len).min(n);
    let cap = delay_samples + 4;
    let mut dl = DelayLine::new(cap);
    let mut dr = DelayLine::new(cap);
    let fb = feedback.clamp(0.0, 0.95);

    // Prime the delay with the tail leading up to `start`.
    let prime_from = start.saturating_sub(delay_samples);
    for i in prime_from..start {
        dl.write(l[i]);
        dr.write(r[i]);
    }

    let d = delay_samples as f32;
    for i in start..end {
        let el = dl.read_lagrange3(d);
        let er = dr.read_lagrange3(d);
        // Feed dry + decayed wet back so repeats build then fade by `feedback`.
        dl.write(l[i] + el * fb);
        dr.write(r[i] + er * fb);
        // Add the echo on top of the dry (keep the mix intact).
        l[i] += el;
        r[i] += er;
    }
}

/// Kick-pump: a grid-locked sidechain "pump" on a melodic bus. Gain dips to
/// `-depth_db` on every beat and recovers exponentially over `release_ms`.
pub fn kick_pump(
    l: &mut [f32],
    r: &mut [f32],
    sr: u32,
    bpm: f64,
    start: usize,
    len: usize,
    depth_db: f32,
    release_ms: f32,
) {
    let n = l.len().min(r.len());
    if start >= n || len == 0 || bpm <= 0.0 {
        return;
    }
    let end = (start + len).min(n);
    let period = (60.0 / bpm * sr as f64).max(1.0); // samples per beat
    let g_min = db_to_lin(-depth_db.abs());
    let tau = (release_ms.max(1.0) * 0.001 * sr as f32).max(1.0);

    for i in start..end {
        // Time since the previous beat boundary (beats measured from `start`).
        let rel = (i - start) as f64;
        let since = rel - (rel / period).floor() * period;
        let env = g_min + (1.0 - g_min) * (1.0 - (-(since as f32) / tau).exp());
        l[i] *= env;
        r[i] *= env;
    }
}

/// Riser: additive white-noise build over `[start, start+len)` — an opening
/// high-pass (`from_hz` → `to_hz`) plus a volume ramp (0 → `peak`, squared
/// for an accelerating swell). Adds into the bus (doesn't replace it).
pub fn riser(
    l: &mut [f32],
    r: &mut [f32],
    sr: u32,
    start: usize,
    len: usize,
    from_hz: f64,
    to_hz: f64,
    peak: f32,
) {
    let n = l.len().min(r.len());
    if start >= n || len == 0 {
        return;
    }
    let end = (start + len).min(n);
    let span = (end - start) as f32;
    let mut rng = Rng::new(0x1a2b_3c4d ^ start as u64);
    let mut hp_l = Biquad::default();
    let mut hp_r = Biquad::default();
    const BLK: usize = 64;

    for k in 0..(end - start) {
        if k % BLK == 0 {
            let t = k as f32 / span;
            // Exponential cutoff sweep (even in log-frequency).
            let cutoff =
                (from_hz as f32) * ((to_hz as f32) / from_hz as f32).powf(t);
            let cutoff = cutoff.clamp(20.0, sr as f32 * 0.45);
            hp_l.set_hpf(sr as f32, cutoff, 0.707);
            hp_r.set_hpf(sr as f32, cutoff, 0.707);
        }
        let t = k as f32 / span;
        let amp = peak * t * t; // squared swell
        let nl = hp_l.process(rng.bipolar());
        let nr = hp_r.process(rng.bipolar());
        l[start + k] += nl * amp;
        r[start + k] += nr * amp;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44_100;

    fn sine(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / SR as f32).sin())
            .collect()
    }
    fn rms(x: &[f32]) -> f32 {
        if x.is_empty() {
            return 0.0;
        }
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn zero_cross_intervals(x: &[f32]) -> Vec<usize> {
        let mut zc = Vec::new();
        let mut last = None;
        for i in 1..x.len() {
            if x[i - 1] <= 0.0 && x[i] > 0.0 {
                if let Some(l) = last {
                    zc.push(i - l);
                }
                last = Some(i);
            }
        }
        zc
    }

    #[test]
    fn tape_stop_drops_pitch() {
        let n = SR as usize;
        let mut l = sine(440.0, 0.8, n);
        let mut r = l.clone();
        let start = 0;
        let len = SR as usize / 2;
        tape_stop(&mut l, &mut r, start, len);
        let zc = zero_cross_intervals(&l[..len]);
        assert!(zc.len() > 4, "need several cycles");
        // Zero-cross spacing (period) must grow: pitch falls.
        let early = zc[1];
        let late = zc[zc.len() - 2];
        assert!(
            late > early * 2,
            "period should grow as tape stops: early {early}, late {late}"
        );
    }

    #[test]
    fn backspin_reverses_and_speeds_up() {
        // A ramp so each sample's source index is identifiable by value.
        let n = SR as usize / 2;
        let mut l: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let mut r = l.clone();
        backspin(&mut l, &mut r, 0, n);
        // First output sample must come from near the END of the source
        // region (backward playback starts at the tail).
        assert!(
            l[0] > (n as f32) * 0.9,
            "backspin should start reading from the tail, got {}",
            l[0]
        );
        // Reverse-rate ramps up: the source-index drop per output sample
        // (in absolute value) should be larger part-way through than at the
        // very start — i.e. it accelerates backward. (Like `tape_stop`, the
        // ramp outruns the source before the region ends and holds at the
        // head from then on, so this checks mid-span, not the tail.)
        let mid = n / 2;
        let early_step = (l[0] - l[1]).abs();
        let late_step = (l[mid] - l[mid + 1]).abs();
        assert!(
            late_step > early_step * 1.5,
            "reverse speed should ramp up: early {early_step}, late {late_step}"
        );
    }

    #[test]
    fn beat_repeat_stutters_the_downbeat() {
        // A ramp so each sample index is identifiable by value.
        let bar = 8000usize;
        let n = bar * 2;
        let mut l: Vec<f32> = (0..n).map(|i| (i % bar) as f32 / bar as f32).collect();
        let mut r = l.clone();
        let src0 = l[..bar].to_vec();
        beat_repeat(&mut l, &mut r, 0, bar);
        let eighth = bar / 8;
        // The last two 1/8 segments must be identical loops of the downbeat.
        let seg_a = &l[bar - 2 * eighth..bar - eighth];
        let seg_b = &l[bar - eighth..bar];
        for k in 0..eighth {
            assert!((seg_a[k] - seg_b[k]).abs() < 1e-6, "1/8 loops differ at {k}");
            // And each is the bar's opening samples.
            assert!((seg_b[k] - src0[k]).abs() < 1e-6, "not the downbeat at {k}");
        }
    }

    #[test]
    fn echo_out_rings_and_decays() {
        let n = SR as usize * 2;
        let start = SR as usize / 2;
        let delay = SR as usize / 8; // 125 ms
        // Content before `start`, silence after.
        let mut l = vec![0.0f32; n];
        let mut r = vec![0.0f32; n];
        for i in (start - delay)..start {
            let v = 0.6 * (2.0 * std::f32::consts::PI * 200.0 * i as f32 / SR as f32).sin();
            l[i] = v;
            r[i] = v;
        }
        echo_out(&mut l, &mut r, start, 8 * delay, delay, 0.6);
        // First echo window right after start vs. a much later one: decays.
        let e1 = rms(&l[start..start + delay]);
        let e3 = rms(&l[start + 3 * delay..start + 4 * delay]);
        assert!(e1 > 1e-4, "first echo should be audible ({e1})");
        assert!(e3 < e1 * 0.7, "echoes should decay: e1 {e1}, e3 {e3}");
        // Beyond the effect window the mix is untouched (stays silent here).
        let after = rms(&l[start + 8 * delay + 100..start + 9 * delay]);
        assert!(after < 1e-5, "echo_out must not touch audio past its window ({after})");
    }

    #[test]
    fn kick_pump_dips_on_each_beat() {
        let bpm = 120.0;
        let n = SR as usize * 2;
        let mut l = vec![0.5f32; n]; // DC-ish steady level to read the envelope
        let mut r = l.clone();
        kick_pump(&mut l, &mut r, SR, bpm, 0, n, 9.0, 120.0);
        let period = (60.0 / bpm * SR as f64) as usize;
        // Just after a beat the level is pushed down; near the next beat it
        // has recovered.
        let just_after = l[period + 200].abs();
        let before_next = l[2 * period - 200].abs();
        assert!(
            just_after < before_next * 0.8,
            "pump should dip after the beat: after {just_after}, before-next {before_next}"
        );
    }

    #[test]
    fn riser_builds_energy_and_brightness() {
        let n = SR as usize;
        let mut l = vec![0.0f32; n];
        let mut r = vec![0.0f32; n];
        riser(&mut l, &mut r, SR, 0, n, 200.0, 12000.0, 0.5);
        let early = rms(&l[..n / 8]);
        let late = rms(&l[7 * n / 8..]);
        assert!(late > early * 3.0, "riser should swell: early {early}, late {late}");
        assert!(late > 0.05, "riser should reach audible level ({late})");
    }

    #[test]
    fn sub_drop_hits_low_and_decays() {
        let n = SR as usize;
        let mut l = vec![0.0f32; n];
        let mut r = vec![0.0f32; n];
        let len = SR as usize / 2;
        sub_drop(&mut l, &mut r, SR, 0, len, 55.0, 45.0, 0.7);
        // Loud at the top, decayed by the tail.
        let head = rms(&l[..len / 8]);
        let tail = rms(&l[7 * len / 8..len]);
        assert!(head > 0.1, "sub-drop should hit ({head})");
        assert!(tail < head * 0.5, "sub-drop should decay: head {head}, tail {tail}");
        // Dominated by low frequency: few zero crossings in the sustain.
        let zc = zero_cross_intervals(&l[SR as usize / 20..len]);
        if !zc.is_empty() {
            let mean = zc.iter().sum::<usize>() as f32 / zc.len() as f32;
            // ~50 Hz at 44.1k → ~880-sample period.
            assert!(mean > 400.0, "sub-drop should be low-freq (period {mean})");
        }
    }

    #[test]
    fn additive_fx_survive_rewrites() {
        // A riser building INTO a beat_repeat rewrite region: the additive fx
        // must be applied after the rewriting ones, or the stutter erases the
        // swell's top (heard as a thin, weightless transition). Silent bed →
        // the only energy at the end must be the riser at full peak.
        let n = SR as usize; // 1 s
        let mut l = vec![0.0f32; n];
        let mut r = vec![0.0f32; n];
        let base = super::ResolvedFx {
            kind: FxKind::Riser,
            start: 0,
            len: n,
            from_hz: 200.0,
            to_hz: 12000.0,
            feedback: 0.0,
            delay_samples: 0,
            depth_db: 0.0,
            release_ms: 0.0,
            peak: 0.5,
        };
        let fx = [
            base,
            super::ResolvedFx { kind: FxKind::BeatRepeat, start: n / 2, len: n / 2, ..base },
        ];
        apply_all(&mut l, &mut r, SR, 120.0, &fx);
        let late = rms(&l[n - n / 20..]);
        // Riser applied last reaches ~peak·noise-RMS ≈ 0.25; a stuttered copy
        // of its midpoint (the bug) stays ≈ 0.07.
        assert!(
            late > 0.18,
            "riser swell must survive the beat_repeat rewrite (late RMS {late})"
        );
    }

    #[test]
    fn hp_sweep_thins_the_low_end() {
        // A low tone should be progressively removed as the HP climbs.
        let n = SR as usize;
        let tone: Vec<f32> = (0..n)
            .map(|i| 0.6 * (2.0 * std::f32::consts::PI * 120.0 * i as f32 / SR as f32).sin())
            .collect();
        let mut l = tone.clone();
        let mut r = tone.clone();
        hp_sweep(&mut l, &mut r, SR, 0, n, 40.0, 4000.0);
        let early = rms(&l[SR as usize / 20..SR as usize / 8]); // HP still low → tone passes
        let late = rms(&l[7 * n / 8..]); // HP well above 120 → tone cut
        assert!(late < early * 0.5, "HP sweep should thin the low tone: early {early}, late {late}");
    }
}
