//! Minimal Standard MIDI File writer (format 0, one track), the same layout `gh.write_mid` makes.
//! In Guitar mode the MIDI channel is the string (string 6 -> channel 0 ... string 1 -> channel 5).

use crate::Note;

const TICKS_PER_BEAT: u32 = 480;

fn vlq(out: &mut Vec<u8>, mut v: u32) {
    let mut buf = [0u8; 5];
    let mut n = 0;
    loop {
        buf[n] = (v & 0x7F) as u8 | if n > 0 { 0x80 } else { 0 };
        n += 1;
        v >>= 7;
        if v == 0 {
            break;
        }
    }
    out.extend(buf[..n].iter().rev());
}

/// MIDI channel for a note: the string in Guitar mode, 0 otherwise.
pub fn channel_of(n: &Note) -> u8 {
    n.string.map_or(0, |s| 6u8.saturating_sub(s).min(5))
}

/// Serialise `notes` (seconds) at `bpm`; the tempo only sets the grid, never the timing.
pub fn write_smf(notes: &[Note], bpm: f64) -> Vec<u8> {
    let bpm = if bpm.is_finite() && bpm > 10.0 { bpm } else { 120.0 };
    let tick = |t: f64| (t.max(0.0) * TICKS_PER_BEAT as f64 * bpm / 60.0).round() as u32;
    // (tick, off-before-on, channel, key, velocity)
    let mut ev: Vec<(u32, u8, u8, u8, u8)> = Vec::with_capacity(notes.len() * 2);
    for n in notes {
        let ch = channel_of(n);
        let on = tick(n.start);
        let off = tick(n.end).max(on + 1);
        ev.push((on, 1, ch, n.midi, n.velocity.clamp(1, 127)));
        ev.push((off, 0, ch, n.midi, 0));
    }
    ev.sort();
    let mut trk: Vec<u8> = Vec::new();
    trk.extend([0x00, 0xFF, 0x51, 0x03]);
    let us_per_beat = (60_000_000.0 / bpm).round() as u32;
    trk.extend(&us_per_beat.to_be_bytes()[1..]);
    let mut last = 0;
    for (t, kind, ch, key, vel) in ev {
        vlq(&mut trk, t - last);
        last = t;
        trk.extend([if kind == 1 { 0x90 } else { 0x80 } | ch, key, vel]);
    }
    trk.extend([0x00, 0xFF, 0x2F, 0x00]);
    let mut out = Vec::with_capacity(trk.len() + 22);
    out.extend(b"MThd");
    out.extend(6u32.to_be_bytes());
    out.extend([0, 0, 0, 1]);
    out.extend((TICKS_PER_BEAT as u16).to_be_bytes());
    out.extend(b"MTrk");
    out.extend((trk.len() as u32).to_be_bytes());
    out.extend(trk);
    out
}

/// Parse back the note-ons and note-offs of a file made by [`write_smf`]
/// (seconds, channel, key, velocity, is_on) — for tests.
pub fn read_smf_events(bytes: &[u8]) -> Option<Vec<(f64, u8, u8, u8, bool)>> {
    if bytes.get(..4)? != b"MThd" {
        return None;
    }
    let tpb = u16::from_be_bytes([bytes[12], bytes[13]]) as f64;
    let mut i = 14;
    if bytes.get(i..i + 4)? != b"MTrk" {
        return None;
    }
    let len = u32::from_be_bytes(bytes[i + 4..i + 8].try_into().ok()?) as usize;
    i += 8;
    let end = i + len;
    let (mut tick, mut us_per_beat, mut out) = (0u64, 500_000.0, Vec::new());
    while i < end {
        let mut dt = 0u32;
        loop {
            let b = *bytes.get(i)?;
            i += 1;
            dt = dt << 7 | (b & 0x7F) as u32;
            if b & 0x80 == 0 {
                break;
            }
        }
        tick += dt as u64;
        let status = *bytes.get(i)?;
        i += 1;
        match status {
            0xFF => {
                let kind = *bytes.get(i)?;
                let l = *bytes.get(i + 1)? as usize;
                if kind == 0x51 && l == 3 {
                    us_per_beat = u32::from_be_bytes([0, bytes[i + 2], bytes[i + 3], bytes[i + 4]]) as f64;
                }
                i += 2 + l;
            }
            s if s & 0xF0 == 0x90 || s & 0xF0 == 0x80 => {
                let secs = tick as f64 / tpb * us_per_beat / 1e6;
                out.push((secs, s & 0x0F, bytes[i], bytes[i + 1], s & 0xF0 == 0x90 && bytes[i + 1] > 0));
                i += 2;
            }
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: f64, end: f64, midi: u8, string: Option<u8>) -> Note {
        Note { start, end, midi, vel: 0.5, velocity: 90, string, fret: string.map(|_| 0) }
    }

    #[test]
    fn round_trip_keeps_time_pitch_and_string_channel() {
        let notes = vec![note(0.5, 1.25, 52, Some(5)), note(1.0, 2.0, 64, Some(1))];
        for bpm in [120.0, 93.0] {
            let ev = read_smf_events(&write_smf(&notes, bpm)).unwrap();
            assert_eq!(ev.len(), 4);
            let on: Vec<_> = ev.iter().filter(|e| e.4).collect();
            assert!((on[0].0 - 0.5).abs() < 0.003 && on[0].1 == 1 && on[0].2 == 52);
            assert!((on[1].0 - 1.0).abs() < 0.003 && on[1].1 == 5 && on[1].2 == 64);
        }
    }

    #[test]
    fn vlq_boundaries() {
        for (v, bytes) in [(0u32, vec![0u8]), (127, vec![0x7F]), (128, vec![0x81, 0x00]), (16384, vec![0x81, 0x80, 0x00])] {
            let mut o = Vec::new();
            vlq(&mut o, v);
            assert_eq!(o, bytes, "{v}");
        }
    }
}
