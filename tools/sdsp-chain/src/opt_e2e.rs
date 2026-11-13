//! End-to-end for `sdsp-chain opt`: the cases of tools/test_mixopt.py on the real renderer, plus a golden
//! comparison with the Python tool. The renderer is the sibling binary of the test executable
//! (`cargo build -p sdsp-chain` first); a missing binary fails the test instead of skipping it.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::opt::{optimize, render, EvalSetup, Evaluator};
use crate::opt_spec::{param_table_cache, parse_loss, parse_vary, Target};

const SR: u32 = 48000;

fn chain_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let bin = exe.parent().unwrap().parent().unwrap().join("sdsp-chain");
    assert!(
        bin.is_file(),
        "build the binary first: cargo build -p sdsp-chain ({})",
        bin.display()
    );
    bin
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sdsp-opt-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tone(hz: f64, amp: f64) -> Vec<f64> {
    (0..(4 * SR) as usize)
        .map(|i| amp * (2.0 * std::f64::consts::PI * hz * i as f64 / SR as f64).sin())
        .collect()
}

fn sum(parts: &[Vec<f64>]) -> Vec<f64> {
    (0..parts[0].len())
        .map(|i| parts.iter().map(|p| p[i]).sum())
        .collect()
}

/// 16-bit stereo PCM, both channels the same (the quantisation tools/test_mixopt.py uses).
fn write_wav(path: &Path, x: &[f64]) {
    let data_len = (x.len() * 4) as u32;
    let mut d = Vec::new();
    d.extend(b"RIFF");
    d.extend((36 + data_len).to_le_bytes());
    d.extend(b"WAVEfmt ");
    d.extend(16u32.to_le_bytes());
    d.extend(1u16.to_le_bytes());
    d.extend(2u16.to_le_bytes());
    d.extend(SR.to_le_bytes());
    d.extend((SR * 4).to_le_bytes());
    d.extend(4u16.to_le_bytes());
    d.extend(16u16.to_le_bytes());
    d.extend(b"data");
    d.extend(data_len.to_le_bytes());
    for v in x {
        let s = ((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes();
        d.extend(s);
        d.extend(s);
    }
    std::fs::write(path, d).unwrap();
}

/// A chord stem (A3 C#4 E4) over a bass stem, one EQ on the chord at 300 Hz.
fn chord_mix(d: &Path, mid_gain: f64) -> toml::Value {
    write_wav(
        &d.join("gtr.wav"),
        &sum(&[tone(220.0, 0.12), tone(277.18, 0.12), tone(329.63, 0.12)]),
    );
    write_wav(
        &d.join("bass.wav"),
        &sum(&[tone(55.0, 0.25), tone(110.0, 0.08)]),
    );
    toml::from_str(&format!(
        "out = \"mix.wav\"\n[[track]]\nname = \"gtr\"\ninput = \"gtr.wav\"\ngain_db = -3.0\n\
         [[track.stage]]\nplugin = \"eq\"\nparams = {{ \"Mid Freq\" = 300.0, \"Mid Gain\" = {mid_gain:?}, \"Mid Q\" = 0.7 }}\n\
         [[track]]\nname = \"bass\"\ninput = \"bass.wav\"\n\
         [[master]]\nplugin = \"limiter\"\nparams = {{ Ceiling = -1.0 }}\n"
    ))
    .unwrap()
}

fn render_ref(cfg: &toml::Value, d: &Path) -> PathBuf {
    let cfg = crate::opt_spec::absolutise(cfg, d);
    render(&chain_bin(), &cfg, &d.join("ref.toml"), &d.join("ref.wav")).unwrap()
}

fn setup(
    cfg: &mut toml::Value,
    d: &Path,
    vary: &[&str],
    loss: &str,
    reference: Option<&Path>,
) -> Evaluator {
    let chain = chain_bin();
    let mut tables = param_table_cache();
    let targets: Vec<Target> = vary
        .iter()
        .map(|s| parse_vary(s, cfg, &chain, &mut tables).unwrap())
        .collect();
    let (terms, _) = parse_loss(loss).unwrap();
    let work = d.join("w");
    Evaluator::new(EvalSetup {
        chain: &chain,
        cfg,
        base_dir: d,
        targets,
        terms,
        reference,
        work: Some(&work),
        span: None,
        ref_span: None,
    })
    .unwrap()
}

fn targets_of(ev: &Evaluator) -> Vec<Target> {
    ev.targets().to_vec()
}

fn term(r: &(f64, crate::opt_spec::Terms), name: &str) -> f64 {
    r.1.vals
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| *v)
        .unwrap()
}

#[test]
fn identity_loss_is_zero_and_the_optimizer_stays_put() {
    let d = scratch("identity");
    let mut cfg = chord_mix(&d, 0.0);
    let reference = render_ref(&cfg, &d);
    let mut ev = setup(
        &mut cfg,
        &d,
        &["gtr.eq.Mid Gain:-12:12"],
        "ref:ref.wav,loudness",
        Some(&reference),
    );
    let loss0 = ev.measure(&[0.0]).unwrap().0;
    assert!(loss0 < 1e-6, "{loss0}");
    let t = targets_of(&ev);
    let out = optimize(&mut ev, &t, 20, 0.25, None, None, true).unwrap();
    assert!(
        out.best.as_ref().unwrap()[0].abs() < 0.5 && out.best_loss < 1e-6,
        "{:?} {}",
        out.best,
        out.best_loss
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn planted_boost_is_undone_within_40_evaluations() {
    let d = scratch("planted");
    let reference = render_ref(&chord_mix(&d, 0.0), &d);
    let mut cfg = chord_mix(&d, 8.0); // the defect: +8 dB at 300 Hz on the chord
    let mut ev = setup(
        &mut cfg,
        &d,
        &["gtr.eq.Mid Gain:-12:12"],
        "ref:ref.wav",
        Some(&reference),
    );
    let t = targets_of(&ev);
    let out = optimize(&mut ev, &t, 40, 0.25, None, None, true).unwrap();
    let base = out.history[0]["loss"].as_f64().unwrap();
    assert!(out.history.len() <= 40);
    assert!(
        out.best.as_ref().unwrap()[0] <= 2.0,
        "undid at least 6 of the 8 dB: {:?}",
        out.best
    );
    assert!(out.best_loss <= 0.3 * base, "{base} -> {}", out.best_loss);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn masking_share_falls_under_a_10_db_cut() {
    let d = scratch("masking");
    let amp = 10f64.powf(-20.0 / 20.0) * 2f64.sqrt();
    write_wav(&d.join("a.wav"), &tone(300.0, amp));
    write_wav(&d.join("b.wav"), &tone(305.0, amp));
    let mut cfg: toml::Value = toml::from_str(
        "[[track]]\nname = \"a\"\ninput = \"a.wav\"\n[[track]]\nname = \"b\"\ninput = \"b.wav\"\n\
         [[track.stage]]\nplugin = \"eq\"\nparams = { \"Mid Freq\" = 300.0, \"Mid Gain\" = 0.0, \"Mid Q\" = 0.7 }\n",
    )
    .unwrap();
    let mut ev = setup(&mut cfg, &d, &["b.eq.Mid Gain:-15:0"], "masking", None);
    let before = ev.measure(&[0.0]).unwrap().0;
    let (hz0, share0) = (ev.last_masking[0].hz, ev.last_masking[0].worst_share);
    let after = ev.measure(&[-10.0]).unwrap().0;
    let share1 = ev.last_masking[0].worst_share;
    assert!(
        share0 > 0.5 && (265..=267).contains(&hz0.0),
        "{hz0:?} {share0}"
    );
    assert!(share1 < 0.2, "{share1}");
    assert!(after < before);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn level_penalty_charges_a_fader_drop_past_2_db_but_not_a_narrow_bell() {
    // Five octaves of tone so the stem carries energy in five bus bands: a fader move drops all of them
    // (the median sees it), a Q 3 bell at 440 Hz drops one.
    let d = scratch("level");
    write_wav(
        &d.join("pad.wav"),
        &sum(&[110.0, 220.0, 440.0, 880.0, 1760.0].map(|hz| tone(hz, 0.1))),
    );
    let mut cfg: toml::Value = toml::from_str(
        "[[track]]\nname = \"pad\"\ninput = \"pad.wav\"\ngain_db = -3.0\n\
         [[track.stage]]\nplugin = \"eq\"\nparams = { \"Mid Freq\" = 440.0, \"Mid Gain\" = 0.0, \"Mid Q\" = 3.0 }\n",
    )
    .unwrap();
    let mut ev = setup(
        &mut cfg,
        &d,
        &["pad.gain_db:-12:0", "pad.eq.Mid Gain:-15:0"],
        "level",
        None,
    );
    let level = |ev: &mut Evaluator, v: [f64; 2]| term(&ev.measure(&v).unwrap(), "level");
    assert!(level(&mut ev, [-3.0, 0.0]).abs() < 1e-6, "the base itself");
    assert!(
        level(&mut ev, [-4.5, 0.0]).abs() < 0.05,
        "1.5 dB down: free"
    );
    assert!(
        (level(&mut ev, [-8.0, 0.0]) - 3.0).abs() < 0.05,
        "5 dB down: pays 3"
    );
    assert!(
        level(&mut ev, [-3.0, -10.0]).abs() < 1e-6,
        "narrow cut: free"
    );
    let _ = std::fs::remove_dir_all(&d);
}

fn opt_cli(d: &Path, extra: &[&str]) -> (i32, String) {
    let out = Command::new(chain_bin())
        .arg("opt")
        .arg(d.join("base.toml"))
        .args(extra)
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
    )
}

#[test]
fn cli_writes_best_toml_render_and_log() {
    let d = scratch("cli");
    let reference = render_ref(&chord_mix(&d, 0.0), &d);
    std::fs::write(
        d.join("base.toml"),
        toml::to_string(&chord_mix(&d, 8.0)).unwrap(),
    )
    .unwrap();
    let (code, text) = opt_cli(
        &d,
        &[
            "--vary",
            "gtr.eq.Mid Gain:-12:12",
            "--loss",
            &format!("ref:{},loudness", reference.display()),
            "--iters",
            "8",
            "--out",
            d.join("best.toml").to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{text}");
    let best: toml::Value =
        toml::from_str(&std::fs::read_to_string(d.join("best.toml")).unwrap()).unwrap();
    assert!(
        best["track"][0]["stage"][0]["params"]["Mid Gain"]
            .as_float()
            .unwrap()
            < 8.0
    );
    assert_eq!(
        best["track"][0]["input"].as_str(),
        Some("gtr.wav"),
        "relative paths kept next to the base"
    );
    assert!(d.join("best.wav").exists());
    let log = std::fs::read_to_string(d.join("best.jsonl")).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert!(
        lines.len() == 9 && lines[0].contains("loss_spec") && lines[8].contains("\"diff\""),
        "{log}"
    );
    assert!(text.contains("Listen before keeping"));
    // --iters 0 measures the base and writes nothing new.
    let (code, text) = opt_cli(
        &d,
        &[
            "--vary",
            "gtr.eq.Mid Gain:-12:12",
            "--loss",
            &format!("ref:{}", reference.display()),
            "--iters",
            "0",
            "--out",
            d.join("zero.toml").to_str().unwrap(),
        ],
    );
    assert_eq!(code, 0, "{text}");
    assert!(!d.join("zero.toml").exists());
    // Bad input is an error with a message, not a panic.
    let (code, text) = opt_cli(&d, &["--vary", "nobody.gain_db:-1:1", "--loss", "masking"]);
    assert_ne!(code, 0);
    assert!(text.contains("no track named 'nobody'"), "{text}");
    let _ = std::fs::remove_dir_all(&d);
}

/// Golden against tools/mixopt.py (numpy + scipy Nelder-Mead), same renderer, same inputs: planted +8 dB,
/// `--vary "gtr.eq.Mid Gain:-12:12" --loss "ref:ref.wav,loudness,masking"`, the first eight evaluations
/// and the best point after 30. Python prints losses to 6 decimals; the tolerance is 2e-3 absolute (meters
/// differ by float summation order, and the fixture's sines come from two libms).
#[test]
fn follows_mixopt_py_evaluation_for_evaluation() {
    let d = scratch("golden");
    let reference = render_ref(&chord_mix(&d, 0.0), &d);
    let mut cfg = chord_mix(&d, 8.0);
    let mut ev = setup(
        &mut cfg,
        &d,
        &["gtr.eq.Mid Gain:-12:12"],
        "ref:ref.wav,loudness,masking",
        Some(&reference),
    );
    let t = targets_of(&ev);
    let out = optimize(&mut ev, &t, 30, 0.25, None, None, true).unwrap();
    let golden = [
        (8.0, 7.494274),
        (2.0, 1.592039),
        (-4.0, 2.631914),
        (-1.0, 0.723147),
        (-4.0, 2.631914),
        (0.5, 0.379428),
        (2.0, 1.592039),
        (-0.25, 0.185365),
    ];
    for (rec, (x, loss)) in out.history.iter().zip(golden) {
        let got = (
            rec["values"]["gtr.eq.Mid Gain"].as_f64().unwrap(),
            rec["loss"].as_f64().unwrap(),
        );
        assert!(
            (got.0 - x).abs() < 1e-3 && (got.1 - loss).abs() < 2e-3,
            "#{}: got {got:?}, python {:?}",
            rec["n"],
            (x, loss)
        );
    }
    assert_eq!(out.history.len(), 30);
    assert!(
        (out.best.as_ref().unwrap()[0] + 0.000977).abs() < 2e-4 && out.best_loss < 2e-3,
        "{:?} {}",
        out.best,
        out.best_loss
    );
    let _ = std::fs::remove_dir_all(&d);
}
