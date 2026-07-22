//! Phase 1 tests — the Spectral (FFT cross-synthesis) engine and the Classic
//! regression guard.
//!
//! The Spectral engine imposes the MODULATOR's magnitude envelope onto the
//! CARRIER's phase spectrum. So with a broadband (white-noise) carrier and a
//! modulator whose energy sits in a narrow formant region, the output spectrum
//! must peak in that same region — and shifting `Formant` up must move that
//! peak up. Classic mode must still vocode the same scene (the band bank opens
//! where the modulator has energy), proving the Phase-1 refactor didn't
//! regress it.
//!
//! Run: `cargo test -p superduper-vocoder --test spectral_mode -- --nocapture`

use superduper_synth_core::analysis::spectrum_with_freq;
use superduper_vocoder::dsp::{
    band_center_hz, VocParams, Vocoder, DETAIL_ULTRA, MAX_BANDS, MAX_VOICES, MODE_CLASSIC,
    MODE_SPECTRAL, PITCH_AUTO, PITCH_VOICE, SRC_INTERNAL, SRC_SIDECHAIN, STFT_LATENCY, WAVE_SAW,
};
use superduper_vocoder::viz::VIZ_CURVE;

const SR: f32 = 48_000.0;
const FORMANT_HZ: f32 = 900.0;

fn base(mode: u32, source: u32) -> VocParams {
    VocParams {
        attack_ms: 3.0,
        release_ms: 25.0,
        source,
        wave: WAVE_SAW,
        band_count: 16,
        pitch_source: PITCH_AUTO,
        notes: [-1; MAX_VOICES],
        pitch_offset_semi: 0.0,
        detune_cents: 0.0,
        formant_semi: 0.0,
        unvoiced: 0.0,
        drive: 0.0,
        mix: 1.0,
        output_lin: 1.0,
        mode,
        detail: 1,
        bypassed: false,
    }
}

/// Modulator = a formant cluster (three sines around `FORMANT_HZ`) so the
/// magnitude envelope has one clear peak. Slow AM keeps band envelopes moving.
fn formant_modulator(n: usize) -> Vec<f32> {
    use std::f32::consts::TAU;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let blob = (TAU * (FORMANT_HZ - 100.0) * t).sin()
                + (TAU * FORMANT_HZ * t).sin()
                + (TAU * (FORMANT_HZ + 100.0) * t).sin();
            let am = 0.6 + 0.4 * (TAU * 4.0 * t).sin();
            blob * 0.2 * am
        })
        .collect()
}

/// A synthetic match for the offline vocaudit `mod_vowels` reference: a loud,
/// dark 130 Hz voice with energy only in the low harmonics (130/260/390/520,
/// steep rolloff above — measured from the real WAV), syllable-rate AM. Its
/// low-concentrated spectrum reproduces the Classic↔Spectral level ratio of the
/// real recording so this makeup calibration is validated in a portable test.
fn vowel_like(n: usize) -> Vec<f32> {
    use std::f32::consts::TAU;
    let f0 = 130.0;
    let amps = [1.0f32, 0.7, 1.3, 0.3]; // h1..h4 — 390 Hz strongest, like the WAV
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let mut s = 0.0;
            for (h, a) in amps.iter().enumerate() {
                s += a * (TAU * f0 * (h + 1) as f32 * t).sin();
            }
            let am = 0.6 + 0.4 * (TAU * 3.0 * t).sin();
            s * 0.3 * am
        })
        .collect()
}

/// A crude sung "voice": 150 Hz fundamental + 8 harmonics, syllable-rate AM.
fn voice_like(n: usize) -> Vec<f32> {
    use std::f32::consts::TAU;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let mut s = 0.0;
            for h in 1..=8 {
                s += (1.0 / h as f32) * (TAU * 150.0 * h as f32 * t).sin();
            }
            let am = 0.5 + 0.5 * (TAU * 3.0 * t).sin();
            s * 0.25 * am
        })
        .collect()
}

fn sine_wave(n: usize, hz: f32) -> Vec<f32> {
    use std::f32::consts::TAU;
    (0..n).map(|i| 0.5 * (TAU * hz * i as f32 / SR).sin()).collect()
}

/// Broadband carrier — deterministic white noise (flat spectrum) so any shape
/// in the output is attributable to the modulator envelope, not the carrier.
fn white_noise(n: usize) -> Vec<f32> {
    let mut s: u32 = 0x1234_5678;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s as f32 / u32::MAX as f32) * 2.0 - 1.0
        })
        .collect()
}

fn lin_spectrum(x: &[f32]) -> Vec<(f32, f32)> {
    spectrum_with_freq(x, SR)
        .into_iter()
        .map(|(hz, db)| (hz, 10f32.powf(db / 20.0)))
        .collect()
}

fn peak_hz(spec: &[(f32, f32)], lo: f32, hi: f32) -> f32 {
    spec.iter()
        .filter(|(hz, _)| *hz >= lo && *hz <= hi)
        .fold((0.0, 0.0), |acc, &(hz, m)| if m > acc.1 { (hz, m) } else { acc })
        .0
}

fn band_energy(spec: &[(f32, f32)], lo: f32, hi: f32) -> f32 {
    spec.iter()
        .filter(|(hz, _)| *hz >= lo && *hz <= hi)
        .map(|(_, m)| m * m)
        .sum()
}

fn band_energy_db(spec: &[(f32, f32)], lo: f32, hi: f32) -> f32 {
    10.0 * band_energy(spec, lo, hi).max(1e-30).log10()
}

/// Naive ramp saw — deliberately bright (harmonics up to Nyquist, a touch of
/// aliasing) to stand in for the internal carrier's "bright HF harmonics" the
/// user flagged as harsh in Spectral. Fed to BOTH engines via the sidechain so
/// the carrier spectrum is identical → the only difference is the engine.
fn bright_saw(n: usize, f0: f32) -> Vec<f32> {
    let dt = f0 / SR;
    let mut phase = 0.0f32;
    (0..n)
        .map(|_| {
            let s = 2.0 * phase - 1.0;
            phase += dt;
            if phase >= 1.0 {
                phase -= 1.0;
            }
            0.4 * s
        })
        .collect()
}

/// Sibilant/broadband modulator: a voiced low body (150 Hz + harmonics) plus a
/// steady band of broadband noise so the 4-10 kHz analysis bands actually open
/// (an "sss" running under the vowel). Syllable-rate AM keeps envelopes moving.
fn sibilant_modulator(n: usize) -> Vec<f32> {
    use std::f32::consts::TAU;
    let mut s: u32 = 0xC0FF_EE11;
    (0..n)
        .map(|i| {
            let t = i as f32 / SR;
            let mut voiced = 0.0;
            for h in 1..=6 {
                voiced += (1.0 / h as f32) * (TAU * 150.0 * h as f32 * t).sin();
            }
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            let noise = (s as f32 / u32::MAX as f32) * 2.0 - 1.0;
            let am = 0.6 + 0.4 * (TAU * 3.0 * t).sin();
            (voiced * 0.18 + noise * 0.35) * am
        })
        .collect()
}

fn centroid(spec: &[(f32, f32)], lo: f32, hi: f32) -> f32 {
    let (num, den) = spec
        .iter()
        .filter(|(hz, _)| *hz >= lo && *hz <= hi)
        .fold((0.0f32, 0.0f32), |(n, d), &(hz, m)| (n + hz * m, d + m));
    if den > 0.0 {
        num / den
    } else {
        0.0
    }
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|&v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// Run the whole file through the vocoder in one block, return the output tail
/// past the STFT latency + warm-up.
fn run(voc: &mut Vocoder, m: &[f32], car: &[f32], p: &VocParams) -> Vec<f32> {
    let n = m.len();
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    voc.process_stereo(m, m, &mut out_l, &mut out_r, car, car, p);
    // Drop latency + a couple of hops of warm-up.
    let skip = (STFT_LATENCY + 4096).min(n);
    out_l[skip..].to_vec()
}

#[test]
fn spectral_mode_transfers_modulator_formant() {
    let n = SR as usize; // 1 s
    let m = formant_modulator(n);
    let car = white_noise(n);

    let mut voc = Vocoder::new(SR);
    let tail = run(&mut voc, &m, &car, &base(MODE_SPECTRAL, SRC_SIDECHAIN));
    assert!(tail.iter().all(|v| v.is_finite()), "spectral output not finite");
    assert!(rms(&tail) > 1e-4, "spectral output too quiet: rms={}", rms(&tail));

    let spec = lin_spectrum(&tail);
    let pk = peak_hz(&spec, 200.0, 6000.0);
    println!("Spectral formant=0 peak = {pk:.0} Hz (expected ~{FORMANT_HZ:.0})");
    assert!(
        (600.0..=1400.0).contains(&pk),
        "spectral output peak {pk:.0} Hz should sit near the {FORMANT_HZ:.0} Hz modulator formant"
    );

    // The flat-spectrum carrier got shaped: energy in the formant band vastly
    // exceeds energy in a distant band.
    let e_formant = band_energy(&spec, 700.0, 1100.0);
    let e_far = band_energy(&spec, 3000.0, 5000.0);
    println!("Spectral formant-band energy = {e_formant:.3}, far-band = {e_far:.3}");
    assert!(
        e_formant > 4.0 * e_far,
        "modulator envelope did not shape the carrier: formant {e_formant:.3} vs far {e_far:.3}"
    );
}

#[test]
fn spectral_formant_shift_moves_the_peak_up() {
    let n = SR as usize;
    let m = formant_modulator(n);
    let car = white_noise(n);

    // Sharp Detail so the formant peak is well-defined for argmax.
    let mut p0 = base(MODE_SPECTRAL, SRC_SIDECHAIN);
    p0.detail = DETAIL_ULTRA;
    let mut voc0 = Vocoder::new(SR);
    let tail0 = run(&mut voc0, &m, &car, &p0);
    let c0 = centroid(&lin_spectrum(&tail0), 200.0, 6000.0);

    let mut p_up = base(MODE_SPECTRAL, SRC_SIDECHAIN);
    p_up.detail = DETAIL_ULTRA;
    p_up.formant_semi = 12.0; // +1 octave → envelope read from k/2 → peak x2
    let mut voc_up = Vocoder::new(SR);
    let tail_up = run(&mut voc_up, &m, &car, &p_up);
    let spec_up = lin_spectrum(&tail_up);
    let c_up = centroid(&spec_up, 200.0, 6000.0);
    let pk_up = peak_hz(&spec_up, 200.0, 6000.0);

    println!("Spectral centroid: formant=0 {c0:.0} Hz, formant=+12 {c_up:.0} Hz, peak_up {pk_up:.0} Hz");
    assert!(
        c_up > c0 * 1.3,
        "Formant +12 should raise the output centroid: {c0:.0} -> {c_up:.0} Hz"
    );
    assert!(
        pk_up > 1300.0,
        "Formant +12 should push the peak above the original formant: {pk_up:.0} Hz"
    );
}

#[test]
fn classic_mode_still_vocodes_after_refactor() {
    // Same scene as the Spectral test: the Classic band bank must open where the
    // modulator has energy, so the noise carrier is shaped toward the formant.
    let n = SR as usize;
    let m = formant_modulator(n);
    let car = white_noise(n);

    let mut voc = Vocoder::new(SR);
    let tail = run(&mut voc, &m, &car, &base(MODE_CLASSIC, SRC_SIDECHAIN));
    assert!(tail.iter().all(|v| v.is_finite()), "classic output not finite");
    assert!(rms(&tail) > 1e-4, "classic output too quiet: rms={}", rms(&tail));

    let spec = lin_spectrum(&tail);
    let pk = peak_hz(&spec, 200.0, 6000.0);
    let e_formant = band_energy(&spec, 700.0, 1100.0);
    let e_far = band_energy(&spec, 3000.0, 5000.0);
    println!("Classic peak = {pk:.0} Hz, formant-band = {e_formant:.3}, far-band = {e_far:.3}");
    assert!(
        (500.0..=1600.0).contains(&pk),
        "Classic band bank should concentrate energy near the {FORMANT_HZ:.0} Hz formant, got {pk:.0} Hz"
    );
    assert!(
        e_formant > 3.0 * e_far,
        "Classic vocoding broken: formant {e_formant:.3} vs far {e_far:.3}"
    );
}

fn peak_abs(x: &[f32]) -> f32 {
    x.iter().map(|v| v.abs()).fold(0.0f32, f32::max)
}
fn rms_db(x: &[f32]) -> f32 {
    20.0 * rms(x).max(1e-9).log10()
}

#[test]
fn spectral_output_has_headroom() {
    // On a realistic voice + saw scene the Spectral wet must sit under 0 dBFS.
    let n = SR as usize;
    let voice = voice_like(n);
    let mut p = base(MODE_SPECTRAL, SRC_INTERNAL); // internal saw carrier
    p.detune_cents = 8.0;
    let mut voc = Vocoder::new(SR);
    let tail = run(&mut voc, &voice, &voice, &p);
    let peak = peak_abs(&tail);
    println!("Spectral output peak = {peak:.4} (must be <= 1.0)");
    assert!(peak <= 1.0, "Spectral output overshoots 0 dBFS: peak={peak:.4}");
}

#[test]
fn classic_spectral_level_match() {
    // Switching Mode must not jump in level. Same vowel+saw scene (mirrors the
    // offline vocaudit reference) through both engines → RMS within ~2 dB, and
    // Spectral must not clip.
    let n = SR as usize;
    let voice = vowel_like(n);
    let mut pc = base(MODE_CLASSIC, SRC_INTERNAL);
    pc.pitch_source = PITCH_VOICE;
    pc.unvoiced = 0.15;
    pc.detune_cents = 8.0;
    let mut ps = base(MODE_SPECTRAL, SRC_INTERNAL);
    ps.pitch_source = PITCH_VOICE;
    ps.unvoiced = 0.15;
    ps.detune_cents = 8.0;

    let mut vc = Vocoder::new(SR);
    let tc = run(&mut vc, &voice, &voice, &pc);
    let mut vs = Vocoder::new(SR);
    let ts = run(&mut vs, &voice, &voice, &ps);

    let (rc, rs) = (rms_db(&tc), rms_db(&ts));
    let (pkc, pks) = (peak_abs(&tc), peak_abs(&ts));
    println!("Classic rms {rc:.1} dB peak {pkc:.3} | Spectral rms {rs:.1} dB peak {pks:.3}");
    assert!(
        (rc - rs).abs() <= 2.0,
        "Mode switch jumps level: Classic {rc:.1} dB vs Spectral {rs:.1} dB (>2 dB)"
    );
    assert!(pks <= 1.0, "Spectral clips: peak {pks:.3}");
}

#[test]
fn spectral_hot_broadband_never_clips() {
    // Worst case for the safety ceiling: loud broadband modulator + loud noise
    // carrier at full unvoiced. Output must still stay ≤ 0 dBFS.
    let n = SR as usize;
    let modu: Vec<f32> = white_noise(n).iter().map(|v| v * 2.0).collect();
    let car: Vec<f32> = white_noise(n).iter().map(|v| v * 2.0).collect();
    let mut p = base(MODE_SPECTRAL, SRC_SIDECHAIN);
    p.unvoiced = 1.0;
    let mut voc = Vocoder::new(SR);
    let tail = run(&mut voc, &modu, &car, &p);
    let pk = peak_abs(&tail);
    println!("Spectral hot-broadband peak = {pk:.4}");
    assert!(tail.iter().all(|v| v.is_finite()), "non-finite output");
    assert!(pk <= 1.0, "Spectral clips on hot broadband: peak {pk:.4}");
}

#[test]
fn classic_viz_bars_light_up_at_the_active_formant() {
    // The Classic band-activity snapshot must show the band(s) nearest the
    // modulator formant opening, and distant bands staying dark.
    let n = SR as usize;
    let m = formant_modulator(n);
    let car = white_noise(n);

    let mut voc = Vocoder::new(SR);
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    voc.process_stereo(&m, &m, &mut out_l, &mut out_r, &car, &car, &base(MODE_CLASSIC, SRC_SIDECHAIN));

    let bars = voc.viz_bars();
    // Nearest band to the 900 Hz formant vs. a distant high band.
    let near = (0..16).min_by(|&a, &b| {
        (band_center_hz(a, 16) - FORMANT_HZ).abs().total_cmp(&(band_center_hz(b, 16) - FORMANT_HZ).abs())
    }).unwrap();
    let far = (0..16).min_by(|&a, &b| {
        (band_center_hz(a, 16) - 4500.0).abs().total_cmp(&(band_center_hz(b, 16) - 4500.0).abs())
    }).unwrap();
    println!(
        "Classic viz: band {near} ({:.0} Hz) = {:.4}, band {far} ({:.0} Hz) = {:.4}",
        band_center_hz(near, 16), bars[near], band_center_hz(far, 16), bars[far]
    );
    assert!(bars[near] > 1e-4, "formant band should light up: {}", bars[near]);
    assert!(
        bars[near] > 3.0 * bars[far] + 1e-9,
        "formant band {} should dominate the distant band {}",
        bars[near],
        bars[far]
    );
    // Sanity: the snapshot array is bounded to the active band count.
    assert!(bars.len() == MAX_BANDS);
}

#[test]
fn spectral_env_curve_peaks_near_the_formant() {
    let n = SR as usize;
    let m = formant_modulator(n);
    let car = white_noise(n);

    // Sharp Detail so the smoothed formant peak is well-localised for argmax.
    let mut p = base(MODE_SPECTRAL, SRC_SIDECHAIN);
    p.detail = DETAIL_ULTRA;
    let mut voc = Vocoder::new(SR);
    let mut out_l = vec![0.0f32; n];
    let mut out_r = vec![0.0f32; n];
    voc.process_stereo(&m, &m, &mut out_l, &mut out_r, &car, &car, &p);

    let mut curve = [0.0f32; VIZ_CURVE];
    voc.write_env_curve(&mut curve);
    // The curve is log-spaced 60 Hz..8 kHz; find the peak's frequency.
    let pk = (0..VIZ_CURVE).max_by(|&a, &b| curve[a].total_cmp(&curve[b])).unwrap();
    let t = pk as f32 / (VIZ_CURVE as f32 - 1.0);
    let pk_hz = 60.0 * (8000.0f32 / 60.0).powf(t);
    println!("Spectral env-curve peak at index {pk} = {pk_hz:.0} Hz");
    assert!(curve[pk] > 1e-5, "env curve must be non-trivial");
    assert!(
        (500.0..=1600.0).contains(&pk_hz),
        "formant-envelope curve should peak near {FORMANT_HZ:.0} Hz, got {pk_hz:.0} Hz"
    );
}

#[test]
fn spectral_preserves_carrier_harmonic_sparsity() {
    // Anti-harshness guard. The old unit-magnitude form filled every FFT bin to
    // unity (dense/buzzy/metallic). The envelope-transfer form keeps the
    // carrier's own magnitude, so a pure-tone carrier + flat (noise) modulator
    // stays concentrated at the carrier tone instead of smearing across band.
    let n = SR as usize;
    let car = sine_wave(n, 500.0);
    let modu = white_noise(n); // flat envelope → should not add formant shape
    let mut p = base(MODE_SPECTRAL, SRC_SIDECHAIN);
    p.unvoiced = 0.0; // isolate the tonal path (no breath)
    let mut voc = Vocoder::new(SR);
    let tail = run(&mut voc, &modu, &car, &p);

    let spec = lin_spectrum(&tail);
    let e_tone = band_energy(&spec, 400.0, 600.0);
    let e_spread = band_energy(&spec, 1000.0, 3000.0);
    println!("Spectral sparsity: tone-band = {e_tone:.4}, spread-band = {e_spread:.4}");
    assert!(e_tone > 1e-4, "carrier tone should survive: {e_tone}");
    assert!(
        e_tone > 20.0 * e_spread,
        "output smeared across the spectrum (buzzy): tone {e_tone:.4} vs spread {e_spread:.4}"
    );
}

#[test]
fn spectral_top_is_rolled_off_not_bare() {
    // Anti-harshness: with a flat (white) carrier + flat modulator the raw
    // envelope-transfer would give a flat, bright top. The HF shelf + darkened
    // breath must roll the top down so it's not a harsh, bare FFT edge.
    let n = SR as usize;
    let modu = white_noise(n);
    let car = white_noise(n);
    let mut p = base(MODE_SPECTRAL, SRC_SIDECHAIN);
    p.unvoiced = 0.15;
    let mut voc = Vocoder::new(SR);
    let tail = run(&mut voc, &modu, &car, &p);
    let spec = lin_spectrum(&tail);
    let mid = band_energy(&spec, 2000.0, 4000.0);
    let top = band_energy(&spec, 11000.0, 13000.0); // same 2 kHz width
    println!("Spectral HF rolloff: mid(2-4k) = {mid:.4}, top(11-13k) = {top:.4}");
    assert!(mid > 1e-5, "mid band should have energy");
    assert!(
        top < 0.5 * mid,
        "top not rolled off (bare/harsh FFT edge): top {top:.4} vs mid {mid:.4}"
    );
}

#[test]
fn spectral_attack_smoothing_keeps_formant_tracking() {
    // The HF temporal smoother must not break formant transfer: the output
    // still concentrates energy at the modulator formant.
    let n = SR as usize;
    let m = formant_modulator(n);
    let car = white_noise(n);
    let mut p = base(MODE_SPECTRAL, SRC_SIDECHAIN);
    p.detail = DETAIL_ULTRA;
    let mut voc = Vocoder::new(SR);
    let tail = run(&mut voc, &m, &car, &p);
    let spec = lin_spectrum(&tail);
    let e_formant = band_energy(&spec, 700.0, 1100.0);
    let e_far = band_energy(&spec, 3000.0, 5000.0);
    assert!(
        e_formant > 4.0 * e_far,
        "attack smoothing broke formant transfer: formant {e_formant:.4} vs far {e_far:.4}"
    );
}

#[test]
fn both_modes_report_the_same_latency() {
    // Classic is intrinsically 0-latency but reports STFT_LATENCY so that
    // switching Mode never re-triggers host PDC. One reported number, both modes.
    let voc = Vocoder::new(SR);
    assert_eq!(voc.latency_samples(), STFT_LATENCY as u32);
}

#[test]
fn classic_output_is_unchanged_by_the_spectral_engine_existing() {
    // Determinism guard: the Classic path is byte-for-byte reproducible and the
    // internal carrier still works (the Phase-1 refactor only moved the band
    // math into a branch + added a latency delay ring).
    let n = 24_000usize;
    let m = formant_modulator(n);
    let car = white_noise(n);

    let mut a = Vocoder::new(SR);
    let mut b = Vocoder::new(SR);
    let p = base(MODE_CLASSIC, SRC_INTERNAL);
    let ta = run(&mut a, &m, &car, &p);
    let tb = run(&mut b, &m, &car, &p);
    assert_eq!(ta.len(), tb.len());
    let max_delta = ta
        .iter()
        .zip(&tb)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(max_delta < 1e-6, "Classic path not deterministic: max delta {max_delta}");
    assert!(rms(&ta) > 1e-4, "Classic internal-carrier output too quiet: rms={}", rms(&ta));
}

#[test]
fn spectral_hf_matches_classic_on_sibilants() {
    // THE anti-harshness acceptance test. A bright saw carrier + a sibilant
    // (voiced-body + broadband-noise) modulator is exactly the scene the user
    // flagged: Classic's mel band bank naturally rolls the top off around 8 kHz,
    // but the raw Spectral envelope-transfer used to pass the whole saw spectrum
    // → the 4-10 kHz top sat ~+16.5 dB above Classic (harsh above 4-5 kHz).
    //
    // We compare the HF **tilt** (4-10 kHz energy relative to the 1-3 kHz body)
    // in each engine — robust to any residual overall-level offset between the
    // two modes, and it's exactly the physical quantity "is the top too hot
    // relative to the rest". Both engines get the identical sidechain carrier,
    // unvoiced=0 so we isolate the tonal HF the audit found (flatness 0.008).
    let n = SR as usize;
    let modu = sibilant_modulator(n);
    let car = bright_saw(n, 110.0);

    let mut pc = base(MODE_CLASSIC, SRC_SIDECHAIN);
    pc.unvoiced = 0.0;
    let mut ps = base(MODE_SPECTRAL, SRC_SIDECHAIN);
    ps.unvoiced = 0.0;

    let mut vc = Vocoder::new(SR);
    let tc = run(&mut vc, &modu, &car, &pc);
    let mut vs = Vocoder::new(SR);
    let ts = run(&mut vs, &modu, &car, &ps);

    let (sc, ss) = (lin_spectrum(&tc), lin_spectrum(&ts));
    let c_hf = band_energy_db(&sc, 4000.0, 10000.0);
    let c_mid = band_energy_db(&sc, 1000.0, 3000.0);
    let s_hf = band_energy_db(&ss, 4000.0, 10000.0);
    let s_mid = band_energy_db(&ss, 1000.0, 3000.0);
    let c_tilt = c_hf - c_mid;
    let s_tilt = s_hf - s_mid;
    let delta = s_tilt - c_tilt;

    // Absolute HF for the record (both modes are level-matched to ~1 dB).
    let abs_delta = s_hf - c_hf;
    println!(
        "HF 4-10k: Classic {c_hf:.1} dB (mid {c_mid:.1}, tilt {c_tilt:.1}) | \
         Spectral {s_hf:.1} dB (mid {s_mid:.1}, tilt {s_tilt:.1})"
    );
    println!("  → HF tilt delta (Spectral-Classic) = {delta:.1} dB, absolute HF delta = {abs_delta:.1} dB");

    // Tightened to the final target: the Spectral top must NOT be hotter than
    // Classic's (the user's harshness complaint) — no more than ~2 dB over, was
    // +16.5 dB. The HF shelf is deliberately tuned so on real voice/sibilant +
    // saw material Spectral's 4-10 kHz sits ≈ Classic (team-lead vocaudit welch:
    // Classic vs Spectral within ~3 dB). This bright-saw torture scene is far
    // brighter than real sources, so the same fixed shelf reads as a strongly
    // negative tilt here — expected; the lower guard only catches a total HF kill.
    assert!(
        delta <= 2.0,
        "Spectral top too hot vs Classic: HF tilt delta {delta:.1} dB (want ≤ 2)"
    );
    assert!(
        delta >= -16.0,
        "Spectral top completely killed (no air): HF tilt delta {delta:.1} dB (want ≥ -16)"
    );
}
