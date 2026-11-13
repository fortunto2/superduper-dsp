//! Where finished transcriptions go: `~/Music/SuperDuper MIDI/<track>-<time>.mid` (+ `.json`).

use std::path::PathBuf;

/// `SUPERDUPER_A2M_OUT` overrides the folder (tests point it at a temp dir).
pub fn default_out_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("SUPERDUPER_A2M_OUT") {
        return PathBuf::from(p);
    }
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default().join("Music").join("SuperDuper MIDI")
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `YYYYMMDD-HHMMSS` in UTC.
pub fn timestamp(unix_secs: u64) -> String {
    let (y, m, d) = civil((unix_secs / 86_400) as i64);
    let s = unix_secs % 86_400;
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", s / 3600, s % 3600 / 60, s % 60)
}

/// A file-system-safe stem from a track name.
pub fn safe_stem(track: &str) -> String {
    let s: String = track.chars().map(|c| if c.is_alphanumeric() || matches!(c, '-' | '_' | ' ') { c } else { '_' }).collect();
    let s = s.trim().trim_matches('_').to_string();
    if s.is_empty() { "audio2midi".into() } else { s.chars().take(60).collect() }
}

/// `<dir>/<track>-<stamp>.mid`, never overwriting: a second analysis in the same second gets `-2`.
pub fn next_path(dir: &std::path::Path, track: &str, unix_secs: u64) -> PathBuf {
    let stem = format!("{}-{}", safe_stem(track), timestamp(unix_secs));
    let mut p = dir.join(format!("{stem}.mid"));
    let mut i = 2;
    while p.exists() {
        p = dir.join(format!("{stem}-{i}.mid"));
        i += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_dates() {
        assert_eq!(timestamp(0), "19700101-000000");
        assert_eq!(timestamp(1_000_000_000), "20010909-014640");
        assert_eq!(timestamp(1_761_840_000), "20251030-160000");
    }

    #[test]
    fn stems_are_safe() {
        assert_eq!(safe_stem("Gtr/DI: take 2"), "Gtr_DI_ take 2");
        assert_eq!(safe_stem(""), "audio2midi");
        assert_eq!(safe_stem("///"), "audio2midi");
    }
}
