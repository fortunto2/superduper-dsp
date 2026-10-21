//! Known-answer controls for the bus meter, one test per control. The list
//! lives in `src/controls.rs` so that `sdsp-bus controls` runs exactly these
//! against the binary's own functions; a control without a test here fails
//! `every_control_is_pinned`. Each scene is a temp bus file with real
//! publishers on it (`SUPERDUPER_BUS`, serialised by the `Bus` lock).

use sdsp_bus::controls::{self, Bus, CLASH_BAND};
use sdsp_bus::serve::Prefer;

/// Pinned ids, in the order the table prints them.
const PINNED: &[&str] = &[
    "null-alone", "null-disjoint", "planted-clash", "planted-half", "planted-buried",
    "prefer-default", "prefer-eq", "fader", "fader-partial", "returns", "empty-masking",
    "phase-cancel", "phase-sum", "phase-limiter", "empty-phase",
];

fn pass(id: &str) {
    match controls::check(id) {
        Ok(measured) => println!("{id}: {measured}"),
        Err(e) => panic!("{id}: {e}"),
    }
}

#[test] fn null_one_track_alone() { pass("null-alone") }
#[test] fn null_two_tracks_in_disjoint_bands() { pass("null-disjoint") }
#[test] fn planted_clash_every_frame() { pass("planted-clash") }
#[test] fn planted_clash_half_the_frames() { pass("planted-half") }
#[test] fn planted_keys_12_db_under_the_bass() { pass("planted-buried") }
#[test] fn prefer_default_is_the_loudest_instance() { pass("prefer-default") }
#[test] fn prefer_names_the_eq_after_the_instrument() { pass("prefer-eq") }
#[test] fn fader_turns_the_clash_into_a_burial() { pass("fader") }
#[test] fn fader_for_gtr_never_moves_gtr_di() { pass("fader-partial") }
#[test] fn returns_and_master_are_never_in_a_pair() { pass("returns") }
#[test] fn no_named_track_is_an_error_not_a_clean_report() { pass("empty-masking") }
#[test] fn phase_finds_the_9_db_cancellation_in_band_8() { pass("phase-cancel") }
#[test] fn phase_sees_nothing_when_the_master_is_the_sum() { pass("phase-sum") }
#[test] fn phase_ignores_a_broadband_limiter() { pass("phase-limiter") }
#[test] fn phase_without_master_or_tracks_is_an_error() { pass("empty-phase") }

#[test]
fn every_control_is_pinned() {
    let ids: Vec<&str> = controls::all().iter().map(|c| c.id).collect();
    assert_eq!(ids, PINNED, "a control was added, renamed or removed without its test");
}

/// A judge that cannot fail is not a control: the planted judge must reject
/// the null scene, and the null judge the planted one.
#[test]
fn the_judges_go_red_on_the_wrong_answer() {
    let mut bus = Bus::new("meta-null");
    controls::scene_disjoint(&mut bus);
    let null = controls::masking(&bus, &Prefer::new(), None).expect("a report");
    let e = controls::judge_one_clash(&null, "Bass", "Keys", CLASH_BAND, 0.95).expect_err("no clash was planted");
    assert!(e.contains("expected exactly 1 clash"), "{e}");
    drop(bus);

    let mut bus = Bus::new("meta-planted");
    controls::scene_planted(&mut bus, -20.0);
    let planted = controls::masking(&bus, &Prefer::new(), None).expect("a report");
    let e = controls::judge_nothing(&planted, &["Bass", "Keys"]).expect_err("a clash was planted");
    assert!(e.contains("expected nothing"), "{e}");
    let e = controls::judge_one_buried(&planted, "Keys", "Bass", CLASH_BAND).expect_err("a clash is not a burial");
    assert!(e.contains("expected no clash"), "{e}");
}

/// The CLI entry prints one line per control and reports all green.
#[test]
fn the_table_runs_green() {
    let mut out = Vec::new();
    let ok = controls::run(&mut out).expect("write");
    let text = String::from_utf8(out).expect("utf8");
    assert!(ok, "{text}");
    assert_eq!(text.matches("\nPASS  ").count(), PINNED.len(), "{text}");
    assert!(!text.contains("FAIL"), "{text}");
}
