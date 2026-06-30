use superduper_synth_core::nam::load_from_json;

/// Minimal, schema-valid WaveNet `.nam` payload kept inline so the test is
/// self-contained. The previous version read
/// `~/.superduper-dsp/nam/test_minimal.nam` — a user-machine file absent from
/// CI and fresh checkouts, so it always panicked on the missing file rather
/// than testing the loader. `load_from_json` only deserialises `NamFile`
/// (architecture + raw config + weights); model-building validation is
/// covered by the `nam-test` tool against real models.
const MINIMAL_WAVENET: &str = r#"{
    "version": "0.5.4",
    "architecture": "WaveNet",
    "config": { "layers": [], "head": null, "head_scale": 1.0 },
    "weights": [0.0, 1.0, -1.0],
    "metadata": null
}"#;

#[test]
fn parses_test_file() {
    let f = load_from_json(MINIMAL_WAVENET).expect("parse minimal WaveNet .nam");
    assert_eq!(f.architecture, "WaveNet");
    assert_eq!(f.weights.len(), 3);
    assert_eq!(f.version.as_deref(), Some("0.5.4"));
}
