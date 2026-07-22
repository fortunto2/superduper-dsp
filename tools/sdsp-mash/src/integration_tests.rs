//! Integration tests — synthetic stems written to disk, referenced by a
//! generated `mash.toml`, rendered through the real decode → align → duck
//! path (no master plugins, so it stays hermetic and fast). Lives inside the
//! binary crate (there is no library target — see `chain.rs` header) and is
//! compiled only under `cfg(test)`.

use std::path::{Path, PathBuf};

use crate::config::{offset_to_samples, MashConfig};
use crate::mix::mix;
use crate::render::{prepare, render_premaster};
use crate::wav_io::write_stereo_f32_wav;

const SR: u32 = 44_100;
const BPM: f64 = 120.0;

/// A unique temp dir for this test process so parallel runs don't collide.
fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sdsp_mash_it_{}_{}", std::process::id(), tag));
    std::fs::create_dir_all(&d).expect("mkdir");
    d
}

fn write_wav(path: &Path, l: &[f32], r: &[f32]) {
    write_stereo_f32_wav(path, l, r, SR).expect("write stem");
}

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

#[test]
fn balance_tames_a_gross_outlier_but_keeps_a_drop() {
    // Two quiet sections define the median; a third is a *gross* outlier
    // (~9 dB up). The deadband leveller pulls the outlier back toward — but
    // not all the way to — the others (a drop within the deadband survives).
    let dir = tmp_dir("balance");
    let n = SR as usize * 6;
    let quiet = sine(150.0, 0.22, n);
    let loud = sine(150.0, 0.62, n); // ~9 dB above the quiet sections
    let qa = dir.join("a.wav");
    let qb = dir.join("b.wav");
    let lc = dir.join("c.wav");
    write_wav(&qa, &quiet, &quiet);
    write_wav(&qb, &quiet, &quiet);
    write_wav(&lc, &loud, &loud);

    let base = format!(
        r#"
bpm = 120
[[section]]
start_sec = 0
[[section.track]]
path = "{a}"
role = "beat-drums"
[[section]]
start_sec = 8
[[section.track]]
path = "{b}"
role = "beat-drums"
[[section]]
start_sec = 16
[[section.track]]
path = "{c}"
role = "beat-drums"
"#,
        a = qa.display(),
        b = qb.display(),
        c = lc.display(),
    );

    let measure = |cfg: &MashConfig| -> (f32, f32) {
        let prep = prepare(cfg).expect("prepare");
        let (mut l, mut r) = mix(&prep.stems, &prep.settings);
        crate::render::level_premaster_sections(cfg, SR, &mut l, &mut r);
        let a = rms(&l[2 * SR as usize..4 * SR as usize]); // quiet section A
        let c = rms(&l[18 * SR as usize..20 * SR as usize]); // loud section C
        (a, c)
    };

    // Balanced (default): the gross outlier is pulled down.
    let cfg = MashConfig::parse(&base).expect("parse");
    let (a_on, c_on) = measure(&cfg);
    let cfg2 = MashConfig::parse(&base.replacen("bpm = 120", "bpm = 120\nbalance_sections = false", 1))
        .expect("parse2");
    let (a_off, c_off) = measure(&cfg2);

    // A is untouched either way; C is clearly quieter with balancing on.
    assert!((a_on - a_off).abs() < a_off * 0.05, "quiet section shouldn't move");
    assert!(
        c_on < c_off * 0.8,
        "gross-outlier section should be pulled down: off {c_off}, on {c_on}"
    );
    // But not flattened all the way to the quiet level (deadband keeps ~4.5 dB).
    assert!(c_on > a_on * 1.3, "a drop within the deadband should still lift: A {a_on}, C {c_on}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn section_beat_has_soft_edges() {
    // A section beat that starts at full amplitude must not slam in at the
    // first sample — the v2.1 micro-fade floor ramps it.
    let dir = tmp_dir("softedge");
    let n = SR as usize * 4;
    let beat = vec![0.6f32; n]; // DC-ish, worst case for clicks
    let bp = dir.join("beat.wav");
    write_wav(&bp, &beat, &beat);
    let text = format!(
        r#"
bpm = 120
[[section]]
name = "A"
start_sec = 0
[[section.track]]
path = "{b}"
role = "beat-drums"
"#,
        b = bp.display(),
    );
    let cfg = MashConfig::parse(&text).expect("parse");
    let prep = prepare(&cfg).expect("prepare");
    let (l, _r) = mix(&prep.stems, &prep.settings);
    // First sample is ramped toward 0; a hard start would be ~0.6.
    assert!(l[0].abs() < 0.15, "beat start should be faded, got {}", l[0]);
    // A little later it's at full level.
    assert!(l[SR as usize].abs() > 0.4, "should reach full level after the fade");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_section_crossfade_overlaps_the_beats() {
    // Two beat sections; the second enters with a crossfade. During the
    // overlap both beats sound, so the boundary shouldn't drop to silence.
    let dir = tmp_dir("sections");
    let n = SR as usize * 4; // 4 s stems
    let beat_a = sine(150.0, 0.4, n);
    let beat_b = sine(90.0, 0.4, n);
    let a_path = dir.join("beatA.wav");
    let b_path = dir.join("beatB.wav");
    write_wav(&a_path, &beat_a, &beat_a);
    write_wav(&b_path, &beat_b, &beat_b);

    // Section A at beat 0; section B at beat 16 with a 4-beat crossfade.
    let text = format!(
        r#"
bpm = {BPM}
[[section]]
name = "A"
start_beat = 0
[[section.track]]
path = "{a}"
role = "beat-drums"
[[section]]
name = "B"
start_beat = 16
transition = "crossfade"
xfade_beats = 4
[[section.track]]
path = "{b}"
role = "beat-drums"
"#,
        a = a_path.display(),
        b = b_path.display(),
    );
    let cfg = MashConfig::parse(&text).expect("parse");
    let prep = prepare(&cfg).expect("prepare");
    let (l, _r) = mix(&prep.stems, &prep.settings);

    // Boundary at beat 16 = 16 * 0.5 s = 8 s.
    let boundary = 8 * SR as usize;
    // Energy through the crossfade region must stay up (no silent gap).
    let xfade_rms = rms(&l[boundary..boundary + SR as usize]);
    let early_rms = rms(&l[SR as usize..2 * SR as usize]);
    assert!(
        xfade_rms > early_rms * 0.5,
        "crossfade must not drop out: early {early_rms}, xfade {xfade_rms}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fx_riser_adds_energy_in_its_window() {
    let dir = tmp_dir("fx");
    let n = SR as usize * 10; // long enough to cover the riser window
    let bed = sine(200.0, 0.1, n);
    let bed_path = dir.join("bed.wav");
    write_wav(&bed_path, &bed, &bed);

    // A riser over beats 8..16 (4..8 s). Compare that window's premaster
    // energy with vs. without the fx applied.
    let text = format!(
        r#"
bpm = {BPM}
[[track]]
path = "{bed}"
role = "beat-drums"
[[fx]]
kind = "riser"
at_beat = 8
len_beats = 8
peak = 0.6
"#,
        bed = bed_path.display(),
    );
    let cfg = MashConfig::parse(&text).expect("parse");
    let prep = prepare(&cfg).expect("prepare");
    let (mut l, mut r) = mix(&prep.stems, &prep.settings);
    // Late in the riser (beat 15 ≈ 7.5 s) where the squared swell is loud.
    let win0 = 15 * SR as usize / 2;
    let win1 = win0 + SR as usize / 4;
    let before = rms(&l[win0..win1]);
    crate::fx::apply_all(&mut l, &mut r, SR, BPM, &prep.fx);
    let after = rms(&l[win0..win1]);
    assert!(!prep.fx.is_empty(), "riser fx should be resolved");
    assert!(after > before * 1.5, "riser should add energy: before {before}, after {after}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vocal_lands_on_the_grid() {
    let dir = tmp_dir("align");
    // Beat bed (steady tone) at offset 0.
    let bed = sine(200.0, 0.2, SR as usize * 2);
    let bed_path = dir.join("drums.wav");
    write_wav(&bed_path, &bed, &bed);

    // Vocal = a single unit impulse at the stem's local sample 0, placed at
    // offset_beats = 8. At 120 BPM that is exactly 4 s = 176_400 samples.
    // Put the impulse past the anti-click micro-fade (v2.1) so it survives.
    let imp: usize = 5000;
    let mut voc = vec![0.0f32; SR as usize];
    voc[imp] = 1.0;
    let voc_path = dir.join("vocal.wav");
    write_wav(&voc_path, &voc, &voc);

    let offset_beats = 8.0;
    let text = format!(
        r#"
bpm = {BPM}
[[track]]
path = "{drums}"
role = "beat-drums"
[[track]]
path = "{vocal}"
role = "vocal"
offset_beats = {offset_beats}
"#,
        drums = bed_path.display(),
        vocal = voc_path.display(),
    );
    let cfg = MashConfig::parse(&text).expect("parse config");
    let (sr, l, _r) = render_premaster(&cfg).expect("render");
    assert_eq!(sr, SR);

    let expected = offset_to_samples(offset_beats, BPM, SR) + imp;
    assert_eq!(offset_to_samples(offset_beats, BPM, SR), 176_400);

    // The impulse rides on top of the bed; it must jump ~1.0 above the bed
    // level at exactly `expected`.
    let bed_here = l[expected - 1].abs();
    assert!(
        (l[expected].abs() - bed_here) > 0.8,
        "vocal impulse not at sample {expected}: here {}, before {}",
        l[expected],
        l[expected - 1]
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn edl_phrase_lands_on_the_grid_through_render() {
    // Full config→render path: a named [[source]] cut by one [[phrase]] must
    // drop its audio at the right absolute grid sample in the premaster.
    let dir = tmp_dir("edl");
    // Beat bed so the mix has a grid.
    let bed = sine(200.0, 0.15, SR as usize * 6);
    let bed_path = dir.join("drums.wav");
    write_wav(&bed_path, &bed, &bed);

    // Source: a lone impulse at 1.0 s. Cut [0.9,1.1) → local peak at 0.1 s.
    let mut src = vec![0.0f32; SR as usize * 2];
    src[SR as usize] = 1.0;
    let src_path = dir.join("voc.wav");
    write_wav(&src_path, &src, &src);

    let text = format!(
        r#"
bpm = {BPM}
[[track]]
path = "{drums}"
role = "beat-drums"
[[source]]
name = "v"
path = "{src}"
[[phrase]]
track = "v"
start_sec = 0.9
end_sec = 1.1
at_beat = 8
"#,
        drums = bed_path.display(),
        src = src_path.display(),
    );
    let cfg = MashConfig::parse(&text).expect("parse");
    let (_, l, _) = render_premaster(&cfg).expect("render");

    // beat 8 @120 = 4 s = 176_400; impulse local 0.1 s = 4_410 → 180_810.
    let expected = 176_400 + 4_410;
    let bed_here = l[expected - 1].abs();
    assert!(
        (l[expected].abs() - bed_here) > 0.8,
        "EDL phrase impulse not at {expected}: here {}, before {}",
        l[expected],
        l[expected - 1],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn source_trim_selects_the_verse_before_the_phrase_engine() {
    // A `[[source]]` with `start_sec`/`len_sec` must be cut to that window
    // BEFORE the phrase engine sees it, so a 3-minute track isn't segmented
    // whole. Proof by construction: the source's only impulse sits at 3.0 s;
    // the source window [2.0, 4.0) s shifts it to trimmed-local 1.0 s, and a
    // phrase cutting [0.9, 1.1) then lands it at beat 8. Without the trim, the
    // same phrase would cut silence (the impulse is far outside [0.9, 1.1) of
    // the untrimmed file) and nothing would appear.
    let dir = tmp_dir("srctrim");
    let bed = sine(200.0, 0.15, SR as usize * 6);
    let bed_path = dir.join("drums.wav");
    write_wav(&bed_path, &bed, &bed);

    let mut src = vec![0.0f32; SR as usize * 6];
    src[3 * SR as usize] = 1.0; // lone impulse at absolute 3.0 s
    let src_path = dir.join("voc.wav");
    write_wav(&src_path, &src, &src);

    let text = format!(
        r#"
bpm = {BPM}
[[track]]
path = "{drums}"
role = "beat-drums"
[[source]]
name = "v"
path = "{src}"
start_sec = 2.0
len_sec = 2.0
[[phrase]]
track = "v"
start_sec = 0.9
end_sec = 1.1
at_beat = 8
"#,
        drums = bed_path.display(),
        src = src_path.display(),
    );
    let cfg = MashConfig::parse(&text).expect("parse");
    let (_, l, _) = render_premaster(&cfg).expect("render");

    // beat 8 @120 = 176_400; trimmed-local impulse 1.0 s, cut-local 0.1 s =
    // 4_410 → absolute 180_810.
    let expected = 176_400 + 4_410;
    let bed_here = l[expected - 1].abs();
    assert!(
        (l[expected].abs() - bed_here) > 0.8,
        "trimmed source impulse not at {expected}: here {}, before {}",
        l[expected],
        l[expected - 1],
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pingpong_renders_vocal_over_the_beat() {
    // A config with [[source]]×2 + [pingpong] must add vocal energy over an
    // otherwise steady beat (the dialogue is placed and summed).
    let dir = tmp_dir("pingpong");
    let n = SR as usize * 8;
    let bed = sine(150.0, 0.2, n);
    let bed_path = dir.join("drums.wav");
    write_wav(&bed_path, &bed, &bed);

    // Two vocal sources: bursts every second so segment_phrases finds phrases.
    let mk = |freq: f32| -> Vec<f32> {
        let mut x = vec![0.0f32; n];
        for p in 0..6 {
            let start = p * SR as usize;
            for k in 0..(SR as usize * 4 / 10) {
                x[start + k] = 0.5 * (2.0 * std::f32::consts::PI * freq * k as f32 / SR as f32).sin();
            }
        }
        x
    };
    let a = mk(400.0);
    let b = mk(600.0);
    let a_path = dir.join("a.wav");
    let b_path = dir.join("b.wav");
    write_wav(&a_path, &a, &a);
    write_wav(&b_path, &b, &b);

    let base = format!(
        r#"
bpm = {BPM}
[[track]]
path = "{drums}"
role = "beat-drums"
"#,
        drums = bed_path.display(),
    );
    let with_pp = format!(
        r#"{base}
[[source]]
name = "a"
path = "{a}"
[[source]]
name = "b"
path = "{b}"
[pingpong]
vocals = ["a", "b"]
phrase_beats = 2
seed = 1
"#,
        a = a_path.display(),
        b = b_path.display(),
    );

    let (_, l_bed, _) = render_premaster(&MashConfig::parse(&base).expect("parse bed")).unwrap();
    let (_, l_pp, _) = render_premaster(&MashConfig::parse(&with_pp).expect("parse pp")).unwrap();

    // The pingpong render carries clearly more energy (the vocal dialogue).
    let e_bed = rms(&l_bed[..n.min(l_bed.len())]);
    let e_pp = rms(&l_pp[..n.min(l_pp.len())]);
    assert!(e_pp > e_bed * 1.1, "pingpong should add vocal energy: bed {e_bed}, pp {e_pp}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ducking_reduces_other_under_vocal() {
    let dir = tmp_dir("duck");
    let n = SR as usize * 2; // 2 s
    // beat-other dominates the bus so its ducking is visible on the summed
    // RMS (a loud vocal would otherwise mask the change).
    let other = sine(220.0, 0.6, n);
    let other_path = dir.join("other.wav");
    write_wav(&other_path, &other, &other);

    // vocal: silent first half, present (well above threshold) second half.
    let mut voc = vec![0.0f32; n];
    let half = n / 2;
    for i in half..n {
        voc[i] = 0.3 * (2.0 * std::f32::consts::PI * 300.0 * i as f32 / SR as f32).sin();
    }
    let voc_path = dir.join("vocal.wav");
    write_wav(&voc_path, &voc, &voc);

    let base = format!(
        r#"
bpm = {BPM}
[[track]]
path = "{other}"
role = "beat-other"
[[track]]
path = "{vocal}"
role = "vocal"
"#,
        other = other_path.display(),
        vocal = voc_path.display(),
    );
    let duck = r#"
[duck]
threshold_db = -35.0
ratio = 8.0
attack_ms = 5.0
release_ms = 60.0
"#;

    let cfg_open = MashConfig::parse(&base).expect("parse open");
    let cfg_duck = MashConfig::parse(&format!("{base}{duck}")).expect("parse duck");

    let (_, l_open, _) = render_premaster(&cfg_open).expect("render open");
    let (_, l_duck, _) = render_premaster(&cfg_duck).expect("render duck");

    // Window well inside the vocal half, past the attack transient.
    let w0 = half + SR as usize / 4;
    let w1 = w0 + SR as usize / 2;
    let open_win = rms(&l_open[w0..w1]);
    let duck_win = rms(&l_duck[w0..w1]);
    assert!(
        duck_win < open_win * 0.8,
        "ducking should cut energy under vocal: open {open_win}, ducked {duck_win}"
    );

    // The vocal-free first half must be identical between the two renders.
    let h0 = SR as usize / 4;
    let h1 = h0 + SR as usize / 2;
    let open_head = rms(&l_open[h0..h1]);
    let duck_head = rms(&l_duck[h0..h1]);
    assert!(
        (open_head - duck_head).abs() < 1e-5,
        "head (no vocal) should be identical: open {open_head}, ducked {duck_head}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
