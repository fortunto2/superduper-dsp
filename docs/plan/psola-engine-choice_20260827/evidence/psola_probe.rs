// Standalone model of the TD-PSOLA grain scheduler, written during the
// cleanup review of the psola-engine-choice track (2026-08-27). It touches no
// repo file and links nothing — `rustc psola_probe.rs -O && ./psola_probe`.
//
// WHY IT IS KEPT: it challenges the premise the whole track was built on.
// Lesson 24 says the smooth-tone defect is a SCOPE defect, so the fix is to
// route around PSOLA. This model reproduces the shipped number (-2.8 dB
// against the repo's -2.6) and then shows that simply DROPPING the per-grain
// epoch snap recovers 33 of the 62 dB, costs nothing on pulsed, voiced or
// breathy material (±1 dB), and still shifts correctly (220 -> 439 Hz):
//
//   source          in     snap      0 st   +12 st  peak@+12  max-step@-12st
//   smooth tone   -64.2   argmax     -2.8     2.4     439 Hz      0.486
//                         none      -35.9   -32.2     439 Hz      0.061
//   pulsed voice  -24.6   argmax    -24.7   -22.7     879 Hz      0.018
//                         none      -25.1   -23.1     879 Hz      0.019
//   voiced        -26.7   argmax    -24.3   -21.0     439 Hz      0.226
//                         none      -23.9   -21.6     439 Hz      0.067
//   breathy        -8.4   argmax     -7.7    -3.3     439 Hz      0.159
//                         none       -8.8    -4.6     439 Hz      0.060
//
// The snap's own justification -- grain edges landing at low-energy points so
// there are no clicks -- is not visible either: at -12 st the snap RAISES the
// max sample step on every source.
//
// CAVEAT, and it is a real one: this model holds t0 fixed at SR/F0, while the
// live engine tracks a smoothed cur_t0 out of YIN. On material whose pitch
// actually moves, the snap may be compensating tracker error, and a
// stationary probe cannot test that. Treat this as "the burden of proof has
// moved onto the snap", not as a finished result.
//
// NOT DISPUTED: routing is still right for POLYPHONY -- PSOLA cannot do it at
// all. The question this raises is only about smooth MONO material.

// Standalone model of synth-core/src/psola.rs's grain scheduler at unity and
// +12 st, to test ONE question: is the smooth-tone noise caused by the epoch
// snap being an INDEPENDENT per-grain decision (fixable) or by the algorithm
// (not fixable)?
//
// Mirrors psola.rs: nominal analysis marks advancing by T0, synthesis marks by
// T0/alpha, 2*T0 Hann grains, weighted OLA normalised by the summed window,
// output read `latency` behind the write head.
//
// Three snap policies:
//   Argmax  — what ships: argmax |x| within +-T0/2 of the nominal mark.
//   None    — no snap at all (pure pitch-synchronous OLA).
//   Tracked — the same argmax, but its OFFSET from the nominal mark is a
//             one-pole-smoothed state (unwrapped mod T0) instead of being
//             re-decided from scratch every grain.

const SR: f32 = 48_000.0;
const F0: f32 = 220.0;
const BLOCK: usize = 256;

#[derive(Copy, Clone, PartialEq)]
enum Snap {
    Argmax,
    None,
    Tracked,
    /// Follow the argmax only while the grain's own window shows a real epoch
    /// (peak/RMS crest excess over sqrt(2)); otherwise freeze the offset. The
    /// same measurement `epoch_sharpness` makes, applied to the snap instead
    /// of to engine selection.
    Gated,
}

fn tone(secs: f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            (1..=6)
                .map(|k| (std::f32::consts::TAU * F0 * k as f32 * t).sin() / k as f32)
                .sum::<f32>()
                * 0.3
        })
        .collect()
}

/// Impulse train through two bandpasses — the pulsed reference source.
fn pulsed(secs: f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    let period = SR / F0;
    let mut b1 = Bp::new(700.0, 5.0);
    let mut b2 = Bp::new(1800.0, 8.0);
    let mut ph = 0.0f32;
    (0..n)
        .map(|_| {
            ph += 1.0;
            let src = if ph >= period {
                ph -= period;
                1.0
            } else {
                0.0
            };
            (b1.p(src) + 0.5 * b2.p(src)) * 0.6
        })
        .collect()
}

/// Rosenberg-pulse voice with aspiration — mirrors `voiced()` in
/// engine_transparency.rs. breath 0.02/open 0.4 = normal note; 0.5/0.7 = the
/// breathy take the router sends to the phase vocoder.
fn voiced(secs: f32, breath: f32, open: f32) -> Vec<f32> {
    let n = (secs * SR) as usize;
    let mut seed = 0x5EED_1234u32;
    let mut rng = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
    };
    let mut bank = [Bp::new(730.0, 9.0), Bp::new(1090.0, 11.0), Bp::new(2440.0, 13.0)];
    let mut air = Bp::new(2600.0, 0.8);
    let mut ph = 0.0f32;
    let mut period = SR / F0;
    let mut amp = 1.0f32;
    let mut prev_g = 0.0f32;
    (0..n)
        .map(|_| {
            ph += 1.0;
            if ph >= period {
                ph -= period;
                period = SR / F0 * (1.0 + 0.004 * rng());
                amp = 1.0 + 0.06 * rng();
            }
            let t = ph / period;
            let (t1, t2) = (open, open * 0.4);
            let g = amp
                * if t < t1 {
                    0.5 * (1.0 - (std::f32::consts::PI * t / t1).cos())
                } else if t < t1 + t2 {
                    (std::f32::consts::FRAC_PI_2 * (t - t1) / t2).cos()
                } else {
                    0.0
                };
            let exc = (g - prev_g) * 40.0;
            prev_g = g;
            let noise = air.p(rng()) * breath;
            let out: f32 = bank.iter_mut().map(|b| b.p(exc + noise)).sum();
            out * 0.5
        })
        .collect()
}

/// RBJ bandpass (constant 0 dB peak), Direct Form I.
struct Bp {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}
impl Bp {
    fn new(hz: f32, q: f32) -> Self {
        let w = std::f32::consts::TAU * hz / SR;
        let (s, c) = (w.sin(), w.cos());
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha;
        Self {
            b0: alpha / a0,
            b1: 0.0,
            b2: -alpha / a0,
            a1: -2.0 * c / a0,
            a2: (1.0 - alpha) / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }
    fn p(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

fn psola(x: &[f32], st: f32, snap: Snap) -> Vec<f32> {
    let t0 = (SR / F0) as f64;
    let t0_max = SR / 70.0;
    let latency = 4 * t0_max.ceil() as usize;
    let guard = 3.0 * t0_max as f64;
    let ring = (latency + BLOCK + 4 * t0_max as usize + 8).next_power_of_two();
    let mask = ring - 1;

    let mut in_ring = vec![0.0f32; ring];
    let mut out_ring = vec![0.0f32; ring];
    let mut win_ring = vec![0.0f32; ring];
    let mut write_pos: usize = 0;
    let mut next_analysis = 0.0f64;
    let mut next_synth = 0.0f64;
    let mut off_smooth = 0.0f64; // Tracked policy state
    let alpha = 2f32.powf(st / 12.0).max(0.05);
    let mut out = vec![0.0f32; x.len()];

    for (i, &v) in x.iter().enumerate() {
        in_ring[write_pos & mask] = v;

        let horizon = write_pos as f64 - guard;
        let mut iters = 0;
        while next_synth < horizon && iters < 64 {
            while next_analysis + t0 <= next_synth {
                next_analysis += t0;
            }
            let nominal = if (next_synth - next_analysis) > (next_analysis + t0 - next_synth) {
                next_analysis + t0
            } else {
                next_analysis
            };

            // --- the epoch snap, three ways ---
            let center = match snap {
                Snap::None => nominal,
                _ => {
                    let search = (t0 * 0.5) as i64;
                    let c0 = nominal.round() as i64;
                    let wp = write_pos as i64;
                    let oldest = wp - mask as i64;
                    let (mut best, mut best_e) = (c0, -1.0f32);
                    let mut sum_sq = 0.0f64;
                    let mut o = -search;
                    while o <= search {
                        let idx = (c0 + o).clamp(oldest, wp);
                        let e = in_ring[(idx as usize) & mask].abs() * 2.0;
                        sum_sq += (e as f64) * (e as f64);
                        if e > best_e {
                            best_e = e;
                            best = idx;
                        }
                        o += 1;
                    }
                    let rms = (sum_sq / (2 * search + 1) as f64).sqrt();
                    let crest_excess =
                        (best_e as f64 / rms.max(1e-12) - std::f64::consts::SQRT_2).max(0.0);
                    if snap == Snap::Argmax {
                        best as f64
                    } else if snap == Snap::Gated {
                        // A real epoch moves the offset; a wandering maximum
                        // does not. 0.4 sits between the two classes measured
                        // per-grain here (pulsed ~1.0, smooth ~0.05).
                        if crest_excess > 0.4 {
                            let mut raw = best as f64 - nominal;
                            while raw - off_smooth > t0 * 0.5 {
                                raw -= t0;
                            }
                            while raw - off_smooth < -t0 * 0.5 {
                                raw += t0;
                            }
                            off_smooth += (raw - off_smooth) * 0.5;
                        }
                        nominal + off_smooth
                    } else {
                        // Same measurement, but it updates a tracked offset
                        // instead of being applied raw. Unwrap into (-T0/2, T0/2]
                        // so the smoother never sees the mod-T0 discontinuity.
                        let mut raw = best as f64 - nominal;
                        while raw - off_smooth > t0 * 0.5 {
                            raw -= t0;
                        }
                        while raw - off_smooth < -t0 * 0.5 {
                            raw += t0;
                        }
                        off_smooth += (raw - off_smooth) * 0.05;
                        nominal + off_smooth
                    }
                }
            };

            // --- place_grain: 2*T0 Hann, weighted OLA ---
            let ti = (t0 as f32).round().max(2.0);
            let l = (2.0 * ti) as usize;
            let inv_l = std::f32::consts::TAU / l as f32;
            let wp = write_pos as i64;
            let oldest = wp - mask as i64;
            for k in 0..l {
                let kf = k as f32;
                let win = 0.5 - 0.5 * (inv_l * kf).cos();
                let in_pos = center + (kf - ti) as f64;
                let ip = in_pos.floor();
                let frac = (in_pos - ip) as f32;
                let i0 = (ip as i64).clamp(oldest, wp);
                let i1 = (i0 + 1).clamp(oldest, wp);
                let s = in_ring[(i0 as usize) & mask]
                    + (in_ring[(i1 as usize) & mask] - in_ring[(i0 as usize) & mask]) * frac;
                let oi = (next_synth + (kf - ti) as f64).round() as i64;
                if oi < 0 {
                    continue;
                }
                let om = (oi as usize) & mask;
                out_ring[om] += s * win;
                win_ring[om] += win;
            }
            next_synth += t0 / alpha as f64;
            iters += 1;
        }

        let rp_abs = write_pos as i64 - latency as i64;
        if rp_abs >= 0 {
            let rp = (rp_abs as usize) & mask;
            let w = win_ring[rp];
            out[i] = if w > 1e-6 { out_ring[rp] / w } else { 0.0 };
            out_ring[rp] = 0.0;
            win_ring[rp] = 0.0;
        }
        write_pos += 1;
    }
    out
}

/// Goertzel magnitude at `hz` over a Hann-windowed segment.
fn goertzel(seg: &[f32], hz: f64) -> f64 {
    let n = seg.len();
    let w = std::f64::consts::TAU * hz / SR as f64;
    let coeff = 2.0 * w.cos();
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    for (i, &v) in seg.iter().enumerate() {
        let win = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
        let s0 = coeff * s1 - s2 + v as f64 * win;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0)
}

/// Noise energy off the harmonic grid of `f0`, relative to harmonic energy, dB.
/// Same shape as engine_transparency.rs's metric, on a 16384-sample window.
fn noise_to_harmonic(x: &[f32], f0: f32) -> f32 {
    let start = (0.6 * SR) as usize;
    let len = 16384usize;
    let seg = &x[start..start + len];
    let bin_hz = SR as f64 / len as f64;
    let (mut harm, mut noise) = (0.0f64, 0.0f64);
    let lo = (60.0 / bin_hz) as usize;
    let hi = (8000.0 / bin_hz) as usize;
    for b in lo..=hi {
        let hz = b as f64 * bin_hz;
        let e = goertzel(seg, hz);
        let near = (1..=36).any(|k| (hz - f0 as f64 * k as f64).abs() < 7.0 * bin_hz);
        if near {
            harm += e;
        } else {
            noise += e;
        }
    }
    10.0 * ((noise / harm.max(1e-30)) as f32).log10()
}

/// Loudest harmonic peak below 1 kHz — proves the shift actually happened.
fn peak_hz(x: &[f32]) -> f32 {
    let start = (0.6 * SR) as usize;
    let seg = &x[start..start + 16384];
    let bin_hz = SR as f64 / 16384.0;
    let (mut best, mut best_e) = (0.0, -1.0);
    for b in ((60.0 / bin_hz) as usize)..=((1000.0 / bin_hz) as usize) {
        let hz = b as f64 * bin_hz;
        let e = goertzel(seg, hz);
        if e > best_e {
            best_e = e;
            best = hz;
        }
    }
    best as f32
}

/// Max |x[n+1]-x[n]| over the steady middle — the repo's click criterion
/// (lesson 19: < 0.4). The epoch snap's documented job is to keep grain edges
/// at low-energy points on DOWNshift, so this is what removing it would cost.
fn max_step(x: &[f32]) -> f32 {
    let start = (0.6 * SR) as usize;
    let end = (1.9 * SR) as usize;
    x[start..end].windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max)
}

fn main() {
    let sources: [(&str, Vec<f32>); 4] = [
        ("smooth tone", tone(2.0)),
        ("pulsed voice", pulsed(2.0)),
        ("voiced", voiced(2.0, 0.02, 0.4)),
        ("breathy", voiced(2.0, 0.5, 0.7)),
    ];
    println!(
        "{:<14} {:>7}  {:<8} {:>10} {:>10} {:>9} {:>12}",
        "source", "in", "snap", "0 st", "+12 st", "peak@+12", "step@-12st"
    );
    for (name, x) in sources.iter() {
        let nin = noise_to_harmonic(x, F0);
        for (label, snap) in [
            ("argmax", Snap::Argmax),
            ("none", Snap::None),
            ("tracked", Snap::Tracked),
            ("gated", Snap::Gated),
        ] {
            let y0 = psola(x, 0.0, snap);
            let y12 = psola(x, 12.0, snap);
            let ydn = psola(x, -12.0, snap);
            println!(
                "{:<14} {:>7.1}  {:<8} {:>10.1} {:>10.1} {:>9.0} {:>12.3}",
                if label == "argmax" { name } else { "" },
                if label == "argmax" { nin } else { f32::NAN },
                label,
                noise_to_harmonic(&y0, F0),
                noise_to_harmonic(&y12, F0 * 2.0),
                peak_hz(&y12),
                max_step(&ydn),
            );
        }
    }
}
