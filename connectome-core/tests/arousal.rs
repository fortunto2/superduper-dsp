//! Background noise is the arousal dial, and a sensory stimulus must not be the thing that
//! keeps the brain alive — otherwise clearing a touch silences it for good, which is the
//! defect this file was written against (measured in the app on 2026-09-14).

use connectome_core::{Graph, Lif, LifParams};

const NEURONS: usize = 700;

fn brain(noise: f32) -> Lif {
    let params = LifParams { noise, weight_scale: 0.15, ..LifParams::default() };
    Lif::new(Graph::synthetic(NEURONS, 12, 1), params, 1)
}

/// Mean fraction of cells firing per step over `steps`.
fn activity(sim: &mut Lif, steps: usize) -> f64 {
    let fired: u32 = sim.run(steps).iter().sum();
    fired as f64 / (steps as f64 * NEURONS as f64)
}

#[test]
fn noise_alone_keeps_a_synthetic_brain_alive() {
    let mut sim = brain(3.5);
    sim.run(2000);
    let a = activity(&mut sim, 1000);
    assert!(a > 0.03 && a < 0.08, "expected the walk band, got {a}");
}

#[test]
fn clearing_a_stimulus_does_not_silence_the_brain() {
    let mut sim = brain(3.5);
    sim.run(2000);
    let before = activity(&mut sim, 1000);
    for i in (0..120).step_by(3) {
        sim.set_stimulus(i, 8.0);
    }
    sim.run(250);
    sim.clear_stimulus();
    sim.run(500);
    let after = activity(&mut sim, 1000);
    assert!(after > before * 0.5, "brain went quiet after the touch: {before} -> {after}");
}

#[test]
fn low_noise_is_sleep_and_a_touch_wakes_it() {
    let mut sim = brain(1.0);
    sim.run(2000);
    assert!(activity(&mut sim, 1000) < 0.002, "noise 1.0 should be silent");
    for i in (0..120).step_by(3) {
        sim.set_stimulus(i, 8.0);
    }
    let peak = sim.run(40).into_iter().max().unwrap() as f64 / NEURONS as f64;
    assert!(peak > 0.03, "a touch on a sleeping brain must fire: peak {peak}");
}

#[test]
fn raising_the_noise_restarts_a_silent_brain() {
    let mut sim = brain(1.0);
    sim.run(2000);
    sim.set_noise(3.5);
    sim.run(1000);
    assert!(activity(&mut sim, 1000) > 0.03);
}

#[test]
fn the_dial_is_graded_not_a_switch() {
    let mut levels = Vec::new();
    for noise in [1.0f32, 2.0, 3.5, 6.0] {
        let mut sim = brain(noise);
        sim.run(2000);
        levels.push(activity(&mut sim, 1000));
    }
    assert!(levels.windows(2).all(|w| w[0] < w[1]), "not monotonic: {levels:?}");
    assert!(levels[1] > 0.004 && levels[1] < 0.02, "noise 2.0 should sit in the groom band: {}", levels[1]);
}
