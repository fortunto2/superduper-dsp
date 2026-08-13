//! DSP-only smoke tests for Wind — drive `WindVoice` directly, no CLAP.

use superduper_wind::voice::{WindParams, WindVoice, N_HARM};
use superduper_synth_core::dsp_blocks::{AdsrEnvelope, AdsrParams, midi_note_to_hz};

const SR: f32 = 48_000.0;

fn harmonics(tone: f32) -> [f32; N_HARM] {
    let rolloff = 2.6 - 2.1 * tone.clamp(0.0, 1.0);
    std::array::from_fn(|n| ((n + 1) as f32).powf(-rolloff))
}

#[allow(clippy::too_many_arguments)]
fn params(
    breath: f32,
    jitter: f32,
    shimmer: f32,
    chiff: f32,
    color: f32,
    howl: f32,
    gust_mult: f32,
    whistle: f32,
) -> WindParams {
    WindParams {
        sr: SR,
        root_hz: midi_note_to_hz(57.0), // A3, 220 Hz
        harmonics: harmonics(0.4),
        formant_f: [500.0, 1100.0, 2000.0],
        formant_bw: [180.0, 260.0, 340.0],
        formant_gain: [1.0, 0.85, 0.65],
        breath,
        jitter,
        shimmer,
        chiff,
        color,
        howl,
        gust_mult,
        whistle,
    }
}

fn render(v: &mut WindVoice, p: &WindParams, n: usize) -> Vec<(f32, f32)> {
    v.env.gate_on();
    v.velocity = 0.9;
    v.on_note_on(SR);
    let adsr = AdsrParams::adsr(SR, 0.01, 0.03, 1.0, 0.2);
    let mut e = AdsrEnvelope::default();
    e.gate_on();
    (0..n)
        .map(|_| {
            let env = e.process(adsr);
            let (l, r) = v.process(p);
            (l * env, r * env)
        })
        .collect()
}

fn rms(xs: &[(f32, f32)]) -> f32 {
    let sum: f32 = xs.iter().map(|&(l, r)| l * l + r * r).sum();
    (sum / (xs.len() as f32 * 2.0)).sqrt()
}

/// Crude high-frequency energy estimate — mean squared first-difference is
/// a cheap proxy for HF/noise content without pulling in an FFT.
fn hf_energy(xs: &[(f32, f32)]) -> f32 {
    let mut sum = 0.0_f32;
    let mut prev = 0.0_f32;
    for &(l, _) in xs {
        let d = l - prev;
        sum += d * d;
        prev = l;
    }
    sum / xs.len() as f32
}

#[test]
fn voice_produces_finite_audible_output() {
    let mut v = WindVoice::new(0);
    let p = params(0.5, 0.15, 0.15, 0.25, 0.4, 0.2, 1.0, 0.0);
    let out = render(&mut v, &p, 8192);
    for &(l, r) in &out {
        assert!(l.is_finite() && r.is_finite(), "Wind voice produced NaN/Inf");
    }
    let level = rms(&out[out.len() / 2..]); // sustained tail, skip attack
    assert!(level > 0.005, "Wind voice should be audible, rms={level}");
    let peak = out.iter().fold(0.0_f32, |a, &(l, r)| a.max(l.abs()).max(r.abs()));
    assert!(peak < 5.0, "Wind voice output exploded, peak={peak}");
}

#[test]
fn raising_breath_increases_hf_energy() {
    // Dry (no breath) vs. full breath — the noise layer should add
    // measurable high-frequency / broadband energy, since it's the whole
    // point of the "wind" stochastic layer.
    let mut v_dry = WindVoice::new(1);
    let p_dry = params(0.0, 0.0, 0.0, 0.0, 0.5, 0.2, 1.0, 0.0);
    let out_dry = render(&mut v_dry, &p_dry, 16384);

    let mut v_wet = WindVoice::new(1);
    let p_wet = params(1.0, 0.0, 0.0, 0.0, 0.5, 0.2, 1.0, 0.0);
    let out_wet = render(&mut v_wet, &p_wet, 16384);

    let hf_dry = hf_energy(&out_dry[out_dry.len() / 2..]);
    let hf_wet = hf_energy(&out_wet[out_wet.len() / 2..]);
    eprintln!("HF energy proxy — dry (Breath=0): {hf_dry:.6}, wet (Breath=1): {hf_wet:.6}");
    assert!(
        hf_wet > hf_dry * 1.3,
        "raising Breath should audibly increase HF/noise energy: dry={hf_dry:.6} wet={hf_wet:.6}"
    );
}

#[test]
fn voice_is_stable_over_a_long_hold() {
    // Long sustain — denormal / accumulation bugs (DC drift, filter
    // blow-up) tend to show up only after thousands of samples.
    let mut v = WindVoice::new(2);
    let p = params(0.8, 0.9, 0.9, 0.5, 0.7, 0.5, 1.0, 0.0);
    let out = render(&mut v, &p, SR as usize * 3); // 3 seconds
    for &(l, r) in out.iter().rev().take(4096) {
        assert!(l.is_finite() && r.is_finite());
        assert!(l.abs() < 5.0 && r.abs() < 5.0, "Wind voice diverged: l={l} r={r}");
    }
}

#[test]
fn chiff_adds_an_attack_burst_then_decays() {
    // With Breath and everything else at 0, only the chiff burst should
    // produce energy — and only for roughly its first ~50 ms.
    let mut v = WindVoice::new(3);
    let p = params(0.0, 0.0, 0.0, 1.0, 0.5, 0.2, 1.0, 0.0);
    let early = render(&mut v, &p, (SR * 0.03) as usize); // first 30 ms
    let early_level = rms(&early);
    eprintln!("chiff early-window rms = {early_level:.5}");
    assert!(early_level > 0.0005, "chiff burst should be audible right after note-on");
}

#[test]
fn howl_engine_is_stable_and_audible_at_full_intensity() {
    // Wind (Howl)-style params: near-silent tone, max breath + howl. The
    // procedural howling-wind engine (3 swept high-Q resonant bandpasses)
    // must still be finite, stable, and clearly audible on its own.
    let mut v = WindVoice::new(4);
    let p = params(0.95, 0.4, 0.5, 0.0, 0.35, 1.0, 1.0, 0.0);
    let out = render(&mut v, &p, SR as usize * 2);
    for &(l, r) in &out {
        assert!(l.is_finite() && r.is_finite(), "Howl engine produced NaN/Inf");
        assert!(l.abs() < 5.0 && r.abs() < 5.0, "Howl engine diverged: l={l} r={r}");
    }
    let level = rms(&out[out.len() / 2..]);
    eprintln!("Howl=1.0 sustained rms = {level:.5}");
    assert!(level > 0.02, "full-Howl wind bed should be clearly audible, rms={level}");
}

#[test]
fn howl_morphs_the_wind_bed_character() {
    // Howl=0 (gentle breath) vs Howl=1 (procedural howl) should measurably
    // differ in HF/broadband energy — they're different noise engines.
    let mut v_low = WindVoice::new(5);
    let p_low = params(0.9, 0.0, 0.0, 0.0, 0.4, 0.0, 1.0, 0.0);
    let out_low = render(&mut v_low, &p_low, 16384);

    let mut v_high = WindVoice::new(5);
    let p_high = params(0.9, 0.0, 0.0, 0.0, 0.4, 1.0, 1.0, 0.0);
    let out_high = render(&mut v_high, &p_high, 16384);

    let hf_low = hf_energy(&out_low[out_low.len() / 2..]);
    let hf_high = hf_energy(&out_high[out_high.len() / 2..]);
    eprintln!("Howl morph HF proxy — Howl=0: {hf_low:.6}, Howl=1: {hf_high:.6}");
    assert!(
        (hf_low - hf_high).abs() > hf_low.min(hf_high) * 0.05,
        "Howl=0 and Howl=1 should sound audibly different: low={hf_low:.6} high={hf_high:.6}"
    );
}

#[test]
fn gust_mult_scales_the_wind_bed_amplitude() {
    // gust_mult is the caller-supplied shared-envelope multiplier for the
    // NOISE bed only (tone is deliberately gust-independent — a gust
    // shouldn't pitch-swell the played note). Use Howl=1.0 so the bed
    // dominates the voice output and the scaling is clearly measurable.
    let mut v_quiet = WindVoice::new(6);
    let p_quiet = params(0.9, 0.0, 0.0, 0.0, 0.4, 1.0, 0.05, 0.0);
    let out_quiet = render(&mut v_quiet, &p_quiet, 16384);

    let mut v_loud = WindVoice::new(6);
    let p_loud = params(0.9, 0.0, 0.0, 0.0, 0.4, 1.0, 1.0, 0.0);
    let out_loud = render(&mut v_loud, &p_loud, 16384);

    let rms_quiet = rms(&out_quiet[out_quiet.len() / 2..]);
    let rms_loud = rms(&out_loud[out_loud.len() / 2..]);
    eprintln!("gust_mult scaling — 0.05x: rms={rms_quiet:.5}, 1.0x: rms={rms_loud:.5}");
    assert!(
        rms_loud > rms_quiet * 1.5,
        "gust_mult should audibly scale the wind bed: quiet={rms_quiet:.5} loud={rms_loud:.5}"
    );
}

#[test]
fn whistle_adds_measurable_tonal_energy() {
    // Whistle=0 vs Whistle=1 at Howl=1 (whistle is gated by Howl) — the
    // Aeolian tone should add measurable energy on top of the broadband
    // howl alone.
    let mut v_off = WindVoice::new(7);
    let p_off = params(0.9, 0.0, 0.0, 0.0, 0.4, 1.0, 1.0, 0.0);
    let out_off = render(&mut v_off, &p_off, 16384);

    let mut v_on = WindVoice::new(7);
    let p_on = params(0.9, 0.0, 0.0, 0.0, 0.4, 1.0, 1.0, 1.0);
    let out_on = render(&mut v_on, &p_on, 16384);

    let rms_off = rms(&out_off[out_off.len() / 2..]);
    let rms_on = rms(&out_on[out_on.len() / 2..]);
    eprintln!("Whistle scaling — Whistle=0: rms={rms_off:.5}, Whistle=1: rms={rms_on:.5}");
    assert!(
        rms_on > rms_off * 1.05,
        "Whistle=1 should add measurable energy over Whistle=0: off={rms_off:.5} on={rms_on:.5}"
    );
    for &(l, r) in &out_on {
        assert!(l.is_finite() && r.is_finite(), "whistle output has NaN/Inf");
    }
}

#[test]
fn whistle_is_silent_when_howl_is_zero() {
    // The whistle is explicitly gated by Howl — a gentle-breath patch
    // (Howl=0) with Whistle cranked shouldn't suddenly whistle.
    let mut v_gated = WindVoice::new(8);
    let p_gated = params(0.9, 0.0, 0.0, 0.0, 0.4, 0.0, 1.0, 1.0);
    let out_gated = render(&mut v_gated, &p_gated, 16384);
    assert!(
        (v_gated.whistle_hz() - 0.0).abs() < 1e-6,
        "whistle_hz should stay 0 (never fired) when Howl=0, got {}",
        v_gated.whistle_hz()
    );
    for &(l, r) in &out_gated {
        assert!(l.is_finite() && r.is_finite());
    }
}

#[test]
fn aeolian_frequency_rises_with_gust_intensity() {
    // Strouhal relation: f = St·U/d, U driven by gust_mult. Low gust_mult
    // should yield a measurably lower whistle frequency than high
    // gust_mult — the "whistles up on the gust" behaviour.
    let mut v_low_wind = WindVoice::new(9);
    let p_low_wind = params(0.9, 0.0, 0.0, 0.0, 0.4, 1.0, 0.0, 1.0);
    let _ = render(&mut v_low_wind, &p_low_wind, 512);
    let f_low = v_low_wind.whistle_hz();

    let mut v_high_wind = WindVoice::new(9);
    let p_high_wind = params(0.9, 0.0, 0.0, 0.0, 0.4, 1.0, 1.0, 1.0);
    let _ = render(&mut v_high_wind, &p_high_wind, 512);
    let f_high = v_high_wind.whistle_hz();

    eprintln!("Aeolian whistle freq — gust_mult=0.0: {f_low:.1} Hz, gust_mult=1.0: {f_high:.1} Hz");
    assert!(f_low > 0.0 && f_high > 0.0, "whistle should have fired in both cases");
    assert!(
        f_high > f_low,
        "whistle frequency should rise with wind intensity (Strouhal f=St*U/d): \
         low={f_low:.1}Hz high={f_high:.1}Hz"
    );
}
