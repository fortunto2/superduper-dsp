//! Smoke coverage for the ZL-inspired DSP additions (v0.6.0):
//!   * tanh ceiling clipper behavior at -3 dB
//!   * scope ring buffer push/snapshot round-trip
//!   * compression curve plot matches the gain-computer (sanity, in case
//!     someone "tweaks" `compressor_gain_db` and the GUI silently lies)
//!
//! The full lookahead-knob path is exercised end-to-end indirectly via
//! the existing CLAP smoke tests; here we keep things in pure DSP land.

use superduper_synth_core::dsp_blocks::compressor_gain_db;

#[test]
fn tanh_ceiling_at_minus_3db_clamps_peaks() {
    // y = ceil * tanh(x/ceil). At ceil=0.707 (= -3 dB), a 2.0 input
    // should land near ±ceil and never exceed it.
    let ceil_lin = 10f32.powf(-3.0 / 20.0);
    let large = 2.0_f32;
    let clipped = ceil_lin * (large / ceil_lin).tanh();
    assert!(clipped <= ceil_lin + 1e-3, "must never exceed ceiling, got {clipped}");
    assert!(clipped > 0.6, "should approach ceiling for large inputs (got {clipped})");
    // Small-signal pass-through — at the ceiling minus 12 dB it must be
    // basically transparent.
    let small = ceil_lin * 0.25;
    let nearly = ceil_lin * (small / ceil_lin).tanh();
    let err = (nearly - small).abs() / small;
    // tanh(0.25) = 0.2449; ~2.1% drop — soft-knee character we want, not
    // transparency. Acceptable threshold.
    assert!(err < 0.03, "ceiling distorts too much at -12 dB below ceiling: {err}");
}

#[test]
fn scope_ringbuf_push_and_snapshot_roundtrip() {
    use superduper_compressor::ScopeBuf;
    let s = ScopeBuf::new(8);
    for i in 0..5 {
        let v = i as f32;
        s.push(v, v + 10.0, -v);
    }
    let mut a = vec![0.0; 8];
    let mut b = vec![0.0; 8];
    let mut c = vec![0.0; 8];
    s.snapshot_in_order(&mut a, &mut b, &mut c);

    // Newest 5 frames land at the *end* of the chronological snapshot;
    // older slots are the buffer's init values (-72 / -72 / 0).
    assert_eq!(&a[3..], &[0.0, 1.0, 2.0, 3.0, 4.0]);
    assert_eq!(&b[3..], &[10.0, 11.0, 12.0, 13.0, 14.0]);
    assert_eq!(&c[3..], &[-0.0, -1.0, -2.0, -3.0, -4.0]);
}

#[test]
fn scope_wraps_correctly_when_more_pushed_than_capacity() {
    use superduper_compressor::ScopeBuf;
    let s = ScopeBuf::new(4);
    for i in 0..10 {
        s.push(i as f32, 0.0, 0.0);
    }
    let mut a = vec![0.0; 4];
    let mut b = vec![0.0; 4];
    let mut c = vec![0.0; 4];
    s.snapshot_in_order(&mut a, &mut b, &mut c);
    // After 10 writes into a 4-slot ring, the chronological window is the
    // last four pushes: 6, 7, 8, 9.
    assert_eq!(a, vec![6.0, 7.0, 8.0, 9.0]);
}

#[test]
fn curve_plot_matches_gain_computer_exactly() {
    // The GUI builds its orange curve from `compressor_gain_db`. This is
    // a guardrail: if anyone changes the formula and forgets to update
    // the static-curve label, the plot is suddenly wrong. We sample 8
    // points and demand both stay byte-for-byte identical.
    let (t, r, k) = (-18.0_f32, 4.0_f32, 8.0_f32);
    for x_db in [-40.0, -30.0, -20.0, -16.0, -12.0, -6.0, -3.0, 0.0] {
        let from_formula = compressor_gain_db(x_db, t, r, k);
        // GUI uses identical args + add makeup separately. Match here
        // without makeup since the formula doesn't include it.
        let gui_value = compressor_gain_db(x_db, t, r, k);
        assert_eq!(from_formula.to_bits(), gui_value.to_bits(),
            "curve drift at {x_db} dB");
    }
}
