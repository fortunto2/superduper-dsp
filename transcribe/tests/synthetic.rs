//! Known-answer test: render plucks of known pitch/time (Karplus-Strong), transcribe, compare.

use superduper_transcribe::{Mode, Settings, Transcriber};

const SR: u32 = 44_100;

fn pluck(buf: &mut [f32], start: f64, midi: u8, dur: f64) {
    let f = 440.0 * 2f64.powf((midi as f64 - 69.0) / 12.0);
    let n = (SR as f64 / f).round() as usize;
    let mut line: Vec<f32> = (0..n).map(|i| (((i * 7919 + midi as usize * 104729) % 2001) as f32 / 1000.0) - 1.0).collect();
    // soften the burst so it is a plucked, not a noisy, string
    for i in 1..n {
        line[i] = 0.5 * (line[i] + line[i - 1]);
    }
    let s0 = (start * SR as f64) as usize;
    let len = (dur * SR as f64) as usize;
    let mut idx = 0;
    for k in 0..len {
        let Some(slot) = buf.get_mut(s0 + k) else { break };
        let cur = line[idx];
        *slot += cur * 0.6;
        let nxt = 0.5 * (line[idx] + line[(idx + 1) % n]) * 0.9985;
        line[idx] = nxt;
        idx = (idx + 1) % n;
    }
}

fn render(notes: &[(f64, u8)]) -> Vec<f32> {
    let mut buf = vec![0.0f32; (SR as f64 * (notes.last().unwrap().0 + 2.5)) as usize];
    for &(t, m) in notes {
        pluck(&mut buf, t, m, 1.6);
    }
    let peak = buf.iter().fold(0.0f32, |a, b| a.max(b.abs()));
    buf.iter_mut().for_each(|x| *x *= 0.7 / peak);
    buf
}

/// (start, midi) pairs in `got` that match each of `want` within 60 ms and the same pitch.
fn recall(want: &[(f64, u8)], got: &[(f64, u8)]) -> usize {
    want.iter().filter(|w| got.iter().any(|g| g.1 == w.1 && (g.0 - w.0).abs() < 0.06)).count()
}

#[test]
fn any_mode_finds_known_plucks() {
    // a slow phrase, one pitch at a time: E2 A2 D3 G3 B3 E4 then back
    let want: Vec<(f64, u8)> = [40u8, 45, 50, 55, 59, 64, 59, 55, 50, 45].iter().enumerate().map(|(i, &m)| (0.5 + i as f64 * 0.9, m)).collect();
    let audio = render(&want);
    let t = Transcriber::new().unwrap();
    let a = t.transcribe(&audio, SR, &Settings::default(), |_| true).unwrap().unwrap();
    let got: Vec<(f64, u8)> = a.notes.iter().map(|n| (n.start, n.midi)).collect();
    let hit = recall(&want, &got);
    eprintln!("any: {hit}/{} found, {} notes total: {got:?}", want.len(), got.len());
    assert!(hit >= 8, "only {hit}/{} plucks found", want.len());
    // The model also reports the bright noisy click of a synthetic pluck as notes above C7
    // (MIDI 93+); inside the playable range there must be few extras.
    let low = got.iter().filter(|g| g.1 <= 84).count();
    assert!(low <= want.len() + 6, "{low} notes <= C6 for {} plucks", want.len());
}

#[test]
fn guitar_mode_names_open_strings() {
    // open strings are unambiguous: low E on string 6 fret 0, ... high E on string 1
    let opens = [(40u8, 6u8), (45, 5), (50, 4), (55, 3), (59, 2), (64, 1)];
    let want: Vec<(f64, u8)> = opens.iter().enumerate().map(|(i, &(m, _))| (0.5 + i as f64 * 1.0, m)).collect();
    let audio = render(&want);
    let t = Transcriber::new().unwrap();
    let s = Settings { mode: Mode::Guitar, ..Settings::default() };
    let a = t.transcribe(&audio, SR, &s, |_| true).unwrap().unwrap();
    let mut right = 0;
    for (&(m, string), &(ts, _)) in opens.iter().zip(&want) {
        if let Some(n) = a.notes.iter().find(|n| n.midi == m && (n.start - ts).abs() < 0.06) {
            eprintln!("midi {m}: string {:?} fret {:?}", n.string, n.fret);
            if n.string == Some(string) && n.fret == Some(0) {
                right += 1;
            }
        } else {
            eprintln!("midi {m}: not found");
        }
    }
    assert!(right >= 5, "{right}/6 open strings named correctly");
}

#[test]
fn capo_shifts_the_fret_not_the_pitch() {
    // G3 on the 3rd string with a capo on 2 is fret 0 of the shifted string; same sound
    let want = vec![(0.5, 57u8), (1.7, 64)];
    let audio = render(&want);
    let t = Transcriber::new().unwrap();
    let s = Settings { mode: Mode::Guitar, capo: 2, max_fret: 12, ..Settings::default() };
    let a = t.transcribe(&audio, SR, &s, |_| true).unwrap().unwrap();
    for n in &a.notes {
        let open = [40u8, 45, 50, 55, 59, 64][6 - n.string.unwrap() as usize];
        assert_eq!(open as i32 + 2 + n.fret.unwrap() as i32, n.midi as i32, "{n:?}");
    }
    assert!(!a.notes.is_empty());
}

#[test]
fn silence_gives_no_notes() {
    let t = Transcriber::new().unwrap();
    for mode in [Mode::Any, Mode::Guitar] {
        let a = t.transcribe(&vec![0.0; 44100 * 4], SR, &Settings { mode, ..Settings::default() }, |_| true).unwrap().unwrap();
        assert!(a.notes.is_empty(), "{mode:?}: {:?}", a.notes);
    }
}
