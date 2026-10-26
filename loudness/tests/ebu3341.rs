//! EBU Tech 3341 (v4, 2016) minimum-requirement cases, the ones a
//! stationary-signal meter can be held to. Tolerance in the spec: ±0.1 LU.
//! "−23 dBFS" there is AES-17: amplitude 10^(−23/20), both channels.

use superduper_loudness::LoudnessMeter;

fn sine_block(m: &mut LoudnessMeter, sr: f32, dbfs: f32, secs: f32, start: &mut usize) {
    let amp = 10f32.powf(dbfs / 20.0);
    let n = (secs * sr) as usize;
    for i in *start..*start + n {
        let s = amp * (std::f32::consts::TAU * 1000.0 * (i as f64 / sr as f64) as f32).sin();
        m.process_stereo(s, s);
    }
    *start += n;
}

fn integrated(sr: f32, blocks: &[(f32, f32)]) -> f32 {
    let mut m = LoudnessMeter::new(sr);
    let mut t = 0;
    for &(db, secs) in blocks {
        sine_block(&mut m, sr, db, secs, &mut t);
    }
    m.integrated_lufs()
}

#[test]
fn case1_minus23_dbfs_reads_minus23_lufs() {
    let i = integrated(48_000.0, &[(-23.0, 20.0)]);
    assert!((i + 23.0).abs() <= 0.1, "EBU 3341 #1: got {i}");
}

#[test]
fn case2_minus33_dbfs_reads_minus33_lufs() {
    let i = integrated(48_000.0, &[(-33.0, 20.0)]);
    assert!((i + 33.0).abs() <= 0.1, "EBU 3341 #2: got {i}");
}

/// #3: −36 / −23 / −36 dBFS for 10 / 60 / 10 s. The quiet ends fall under the
/// relative gate (−10 LU), so the programme reads the loud middle.
#[test]
fn case3_relative_gate_ignores_quiet_ends() {
    let i = integrated(48_000.0, &[(-36.0, 10.0), (-23.0, 60.0), (-36.0, 10.0)]);
    assert!((i + 23.0).abs() <= 0.1, "EBU 3341 #3: got {i}");
}

/// #4: −72 / −36 / −23 / −36 / −72 dBFS, 10 / 10 / 60 / 10 / 10 s. Adds the
/// absolute gate (−70 LUFS) on top of #3.
#[test]
fn case4_absolute_gate_drops_near_silence() {
    let i = integrated(
        48_000.0,
        &[(-72.0, 10.0), (-36.0, 10.0), (-23.0, 60.0), (-36.0, 10.0), (-72.0, 10.0)],
    );
    assert!((i + 23.0).abs() <= 0.1, "EBU 3341 #4: got {i}");
}

/// Video audio is often 44.1 kHz; the K-weighting is re-derived there, so the
/// calibration must hold off 48 kHz too (the module promises < 0.05 LU).
#[test]
fn calibration_holds_at_44k1() {
    let i = integrated(44_100.0, &[(-23.0, 20.0)]);
    assert!((i + 23.0).abs() <= 0.1, "44.1 kHz: got {i}");
}
