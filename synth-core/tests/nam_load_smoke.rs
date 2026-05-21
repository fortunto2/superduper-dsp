use superduper_synth_core::nam::load_from_json;

#[test]
fn parses_test_file() {
    let p = std::path::PathBuf::from(std::env::var("HOME").unwrap())
        .join(".superduper-dsp/nam/test_minimal.nam");
    let s = std::fs::read_to_string(&p).expect("read");
    let f = load_from_json(&s).expect("parse");
    assert_eq!(f.architecture, "WaveNet");
}
