//! The plugin on this machine's real `~/Music/SFZ` — the path a DAW takes:
//! fresh instance, loader thread picks the preferred default (Salamander),
//! decodes it, and a note sounds. Skips when no packs are installed; set
//! SDSP_REQUIRE_PACKS=1 to make that a failure.

use clack_host::events::event_types::{NoteOffEvent, NoteOnEvent};
use clack_host::events::Pckn;
use sdsp_test_kit::host::render_with;
use sdsp_test_kit::probes::{self, db};
use sdsp_test_kit::PluginUnderTest;
use std::sync::atomic::Ordering;

struct Target;
impl PluginUnderTest for Target {
    type Plugin = superduper_piano::SuperDuperPiano;
    const ID: &'static std::ffi::CStr = c"co.superduperai.piano";
}

#[test]
fn default_pack_loads_and_plays() {
    let root = superduper_synth_core::sfz::load::default_pack_root();
    let lib = superduper_synth_core::sfz::load::scan_packs(&root);
    if lib.is_empty() {
        eprintln!("SKIP: no .sfz under {}", root.display());
        assert!(std::env::var("SDSP_REQUIRE_PACKS").is_err(), "packs required");
        return;
    }
    std::env::remove_var("SUPERDUPER_PIANO_ROOT");
    let before = superduper_piano::LOADS_COMPLETED.load(Ordering::SeqCst);
    let t0 = std::time::Instant::now();
    let sr = 48_000.0;
    let frames = (sr * 2.0) as usize;
    let silence = vec![0.0f32; frames];
    let (l, _) = render_with::<Target>(sr, 512, &silence, &silence, move |pos, buf| {
        if pos == 0 {
            while superduper_piano::LOADS_COMPLETED.load(Ordering::SeqCst) == before && t0.elapsed().as_secs() < 30 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        if pos == 512 { buf.push(&NoteOnEvent::new(0, Pckn::new(0u16, 0u16, 60u16, 0u32), 0.8)); }
        if pos == 512 * 90 { buf.push(&NoteOffEvent::new(0, Pckn::new(0u16, 0u16, 60u16, 0u32), 0.0)); }
    });
    let load_s = t0.elapsed().as_secs_f64();
    let level = db(probes::rms(&l));
    eprintln!("{} entries; loaded in ≤{load_s:.1} s; C4 v0.8 RMS {level:.1} dB", lib.len());
    assert!(superduper_piano::LOADS_COMPLETED.load(Ordering::SeqCst) > before, "loader never finished");
    assert!(level > -40.0, "C4 is silent through the plugin: {level:.1} dB");
}
