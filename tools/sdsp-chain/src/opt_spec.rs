//! The recipe half of `sdsp-chain opt`: what may move (`--vary`), what is measured (`--loss`), and how a
//! candidate becomes a config. Port of the whitelist/loss/stem parts of tools/mixopt.py.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::Command;

use toml::{Table, Value};

#[derive(Clone, Default)]
pub struct Terms {
    pub vals: Vec<(&'static str, f64)>,
    pub levels: Option<BTreeMap<String, f64>>,
}

#[derive(Clone, Debug)]
pub struct Target {
    pub key: String,
    /// Track name or "master".
    pub track: String,
    /// Plugin key, or None for a track field (gain_db, pan).
    pub stage: Option<String>,
    /// Which stage of that plugin on the track, 0-based.
    pub nth: usize,
    pub param: String,
    pub lo: f64,
    pub hi: f64,
    pub start: f64,
    /// The key actually written into params (name or numeric id).
    pub param_key: String,
}

impl Target {
    pub fn clip(&self, v: f64) -> f64 {
        v.clamp(self.lo, self.hi)
    }
}

/// Python's `%.{prec}g`.
pub fn fmt_g(x: f64, prec: usize) -> String {
    if x == 0.0 || !x.is_finite() {
        return if x == 0.0 { "0".into() } else { format!("{x}") };
    }
    let exp = x.abs().log10().floor() as i32;
    let sci = format!("{:.*e}", prec - 1, x);
    let (mant, e) = sci.split_once('e').unwrap_or((&sci, "0"));
    let e: i32 = e.parse().unwrap_or(exp);
    let trim = |s: &str| {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if e < -4 || e >= prec as i32 {
        format!(
            "{}e{}{:02}",
            trim(mant),
            if e < 0 { '-' } else { '+' },
            e.abs()
        )
    } else {
        trim(&format!("{:.*}", (prec as i32 - 1 - e).max(0) as usize, x))
    }
}

pub fn g3(x: f64) -> String {
    fmt_g(x, 3)
}

pub fn g4(x: f64) -> String {
    fmt_g(x, 4)
}

// ---------------------------------------------------------------------------
// Plugin parameter tables (`sdsp-chain --params <plugin>`)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ParamRow {
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
}

pub type Tables = HashMap<String, Vec<ParamRow>>;

pub fn param_table_cache() -> Tables {
    HashMap::new()
}

/// One row: `id  name with spaces  min  max  default  [unit]`.
fn parse_param_row(line: &str) -> Option<ParamRow> {
    let t: Vec<&str> = line.split_whitespace().collect();
    let id = t.first()?.parse::<u32>().ok()?;
    let mut end = t.len();
    if t.last()?.parse::<f64>().is_err() {
        end -= 1;
    }
    if end < 5 {
        return None;
    }
    let nums: Vec<f64> = t[end - 3..end]
        .iter()
        .map(|x| x.parse::<f64>().ok())
        .collect::<Option<_>>()?;
    Some(ParamRow {
        id,
        name: t[1..end - 3].join(" "),
        min: nums[0],
        max: nums[1],
        default: nums[2],
    })
}

pub fn param_table<'a>(
    chain: &Path,
    plugin: &str,
    cache: &'a mut Tables,
) -> Result<&'a [ParamRow], String> {
    if !cache.contains_key(plugin) {
        let r = Command::new(chain)
            .args(["--params", plugin])
            .output()
            .map_err(|e| e.to_string())?;
        if !r.status.success() {
            let msg = String::from_utf8_lossy(if r.stderr.is_empty() {
                &r.stdout
            } else {
                &r.stderr
            })
            .trim()
            .to_string();
            return Err(format!("sdsp-chain --params {plugin}: {msg}"));
        }
        let rows: Vec<ParamRow> = String::from_utf8_lossy(&r.stdout)
            .lines()
            .filter(|l| !l.trim_start().starts_with("id "))
            .filter_map(parse_param_row)
            .collect();
        if rows.is_empty() {
            return Err(format!("could not parse `sdsp-chain --params {plugin}`"));
        }
        cache.insert(plugin.to_string(), rows);
    }
    Ok(&cache[plugin])
}

// ---------------------------------------------------------------------------
// Whitelist: which (track, stage, param) may move, and between what
// ---------------------------------------------------------------------------

fn tracks_of(cfg: &Value) -> Vec<&Table> {
    cfg.get("track")
        .and_then(|t| t.as_array())
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_table())
        .collect()
}

/// The table a target writes into: a track (for gain_db/pan) or one of its stages.
fn node_mut<'a>(cfg: &'a mut Value, t: &Target) -> Result<&'a mut Table, String> {
    let names: Vec<String> = tracks_of(cfg)
        .iter()
        .map(|x| {
            x.get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("?")
                .to_string()
        })
        .collect();
    let owner: &mut Value = if t.track == "master" {
        cfg.get_mut("master").ok_or(format!(
            "--vary {}: the config has no [[master]] chain",
            t.key
        ))?
    } else {
        let i = names.iter().position(|n| *n == t.track).ok_or(format!(
            "--vary {}: no track named '{}' (have: {})",
            t.key,
            t.track,
            names.join(", ")
        ))?;
        let tr = cfg
            .get_mut("track")
            .and_then(|v| v.as_array_mut())
            .and_then(|a| a.get_mut(i))
            .ok_or("track list vanished")?;
        if t.stage.is_none() {
            return tr
                .as_table_mut()
                .ok_or_else(|| "track is not a table".to_string());
        }
        tr.get_mut("stage").ok_or(format!(
            "--vary {}: '{}' has 0 '{}' stage(s), wanted #{}",
            t.key,
            t.track,
            t.stage.as_deref().unwrap_or(""),
            t.nth + 1
        ))?
    };
    let plugin = t.stage.as_deref().unwrap_or("");
    let stages = owner.as_array_mut().ok_or("stage list is not an array")?;
    let hits = stages
        .iter()
        .filter(|s| s.get("plugin").and_then(|p| p.as_str()) == Some(plugin))
        .count();
    stages
        .iter_mut()
        .filter(|s| s.get("plugin").and_then(|p| p.as_str()) == Some(plugin))
        .nth(t.nth)
        .and_then(|s| s.as_table_mut())
        .ok_or(format!(
            "--vary {}: '{}' has {hits} '{plugin}' stage(s), wanted #{}",
            t.key,
            t.track,
            t.nth + 1
        ))
}

fn as_f64(v: &Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// 'gtr.eq.Mid Gain:-12:0' -> Target with start value and real bounds.
pub fn parse_vary(
    spec: &str,
    cfg: &mut Value,
    chain: &Path,
    tables: &mut Tables,
) -> Result<Target, String> {
    let bad = || format!("--vary '{spec}': want <track>.<stage>.<param>:<lo>:<hi>");
    let mut it = spec.rsplitn(3, ':');
    let (hi, lo, key) = (
        it.next().ok_or_else(bad)?,
        it.next().ok_or_else(bad)?,
        it.next().ok_or_else(bad)?,
    );
    let (lo, hi) = (
        lo.parse::<f64>().map_err(|_| bad())?,
        hi.parse::<f64>().map_err(|_| bad())?,
    );
    if hi <= lo {
        return Err(format!("--vary '{spec}': hi must exceed lo"));
    }
    let parts: Vec<&str> = key.split('.').collect();
    let mut t = Target {
        key: key.to_string(),
        track: String::new(),
        stage: None,
        nth: 0,
        param: String::new(),
        lo,
        hi,
        start: 0.0,
        param_key: String::new(),
    };
    if parts.len() == 2 {
        (t.track, t.param) = (parts[0].to_string(), parts[1].to_string());
        if t.param != "gain_db" && t.param != "pan" {
            return Err(format!("--vary '{spec}': track fields are gain_db or pan"));
        }
        t.start = node_mut(cfg, &t)?
            .get(&t.param)
            .and_then(as_f64)
            .unwrap_or(0.0);
        t.param_key = t.param.clone();
        return Ok(t);
    }
    if parts.len() < 3 {
        return Err(format!("--vary '{spec}': want <track>.<stage>.<param>"));
    }
    let (mut stage, mut nth) = (parts[parts.len() - 2].to_string(), 0);
    if let Some((s, n)) = stage.clone().split_once('#') {
        stage = s.to_string();
        nth = n.parse::<usize>().map_err(|_| bad())?.saturating_sub(1);
    }
    (t.track, t.stage, t.nth, t.param) = (
        parts[..parts.len() - 2].join("."),
        Some(stage),
        nth,
        parts[parts.len() - 1].to_string(),
    );
    finish_stage_target(spec, t, cfg, chain, tables)
}

fn finish_stage_target(
    spec: &str,
    mut t: Target,
    cfg: &mut Value,
    chain: &Path,
    tables: &mut Tables,
) -> Result<Target, String> {
    let stage = t.stage.clone().unwrap_or_default();
    node_mut(cfg, &t)?;
    let rows = param_table(chain, &stage, tables)?;
    let row = rows
        .iter()
        .find(|r| r.name == t.param)
        .or_else(|| rows.iter().find(|r| r.name.eq_ignore_ascii_case(&t.param)))
        .cloned()
        .ok_or_else(|| {
            format!(
                "--vary '{spec}': {stage} has no param '{}'. Names: {}",
                t.param,
                rows.iter()
                    .map(|r| r.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    let node = node_mut(cfg, &t)?;
    let params = node.get("params").and_then(|p| p.as_table());
    t.param_key = params
        .and_then(|p| {
            p.keys()
                .find(|k| {
                    **k == row.name
                        || k.eq_ignore_ascii_case(&row.name)
                        || **k == row.id.to_string()
                })
                .cloned()
        })
        .unwrap_or(row.name.clone());
    t.start = params
        .and_then(|p| p.get(&t.param_key))
        .and_then(as_f64)
        .unwrap_or(row.default);
    let (clo, chi) = (t.lo.max(row.min), t.hi.min(row.max));
    if (clo, chi) != (t.lo, t.hi) {
        println!(
            "  note: {} bounds {}:{} clipped to the plugin's {}:{}",
            t.key, t.lo, t.hi, clo, chi
        );
    }
    (t.lo, t.hi) = (clo, chi);
    if !(clo <= t.start && t.start <= chi) {
        println!(
            "  note: {} starts at {} outside {clo}:{chi}; the first point is clipped",
            t.key, t.start
        );
    }
    Ok(t)
}

pub fn apply_values(cfg: &Value, targets: &[Target], values: &[f64]) -> Result<Value, String> {
    let mut out = cfg.clone();
    for (t, v) in targets.iter().zip(values) {
        let node = node_mut(&mut out, t)?;
        if t.stage.is_some() {
            let params = node
                .entry("params")
                .or_insert_with(|| Value::Table(Table::new()));
            if let Some(p) = params.as_table_mut() {
                p.insert(t.param_key.clone(), Value::Float(*v));
            }
        } else {
            node.insert(t.param_key.clone(), Value::Float(*v));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Loss spec
// ---------------------------------------------------------------------------

/// 'ref:ref.wav,masking=2,loudness' -> ([(term, weight)], ref path).
pub fn parse_loss(spec: &str) -> Result<(Vec<(String, f64)>, Option<String>), String> {
    let mut terms: Vec<(String, f64)> = vec![];
    let mut reference = None;
    for raw in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(r) = raw.strip_prefix("ref:") {
            reference = Some(r.to_string());
            if !terms.iter().any(|(k, _)| k == "band") {
                terms.push(("band".into(), 1.0));
            }
            continue;
        }
        let (name, w) = raw.split_once('=').unwrap_or((raw, ""));
        if !["band", "loudness", "masking", "level"].contains(&name) {
            return Err(format!(
                "--loss: unknown term '{name}' (ref:<file>, loudness, masking, level)"
            ));
        }
        let weight = if w.is_empty() {
            1.0
        } else {
            w.parse::<f64>()
                .map_err(|_| format!("--loss: weight of {name} is not a number"))?
        };
        match terms.iter_mut().find(|(k, _)| k == name) {
            Some(t) => t.1 = weight,
            None => terms.push((name.to_string(), weight)),
        }
    }
    if terms.is_empty() {
        return Err("--loss: nothing to measure".into());
    }
    if terms.iter().any(|(k, _)| k == "band" || k == "loudness") && reference.is_none() {
        return Err("--loss: band/loudness need ref:<wav or check json>".into());
    }
    Ok((terms, reference))
}

// ---------------------------------------------------------------------------
// Stems
// ---------------------------------------------------------------------------

/// sdsp-chain resolves `input`/file sidechains against the CWD: pin them to the base config's folder.
pub fn absolutise(cfg: &Value, base_dir: &Path) -> Value {
    let fix = |v: &mut Value| {
        if let Some(s) = v.as_str().map(String::from) {
            if !(s.starts_with("track:") || s.is_empty() || Path::new(&s).is_absolute()) {
                *v = Value::String(base_dir.join(s).display().to_string());
            }
        }
    };
    let mut out = cfg.clone();
    let Some(root) = out.as_table_mut() else {
        return out;
    };
    root.get_mut("sidechain").map(fix);
    for tr in root
        .get_mut("track")
        .and_then(|t| t.as_array_mut())
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_table_mut())
    {
        tr.get_mut("input").map(fix);
        tr.get_mut("sidechain").map(fix);
        for st in tr
            .get_mut("stage")
            .and_then(|s| s.as_array_mut())
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_table_mut())
        {
            st.get_mut("sidechain").map(fix);
        }
    }
    for st in root
        .get_mut("master")
        .and_then(|s| s.as_array_mut())
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_table_mut())
    {
        st.get_mut("sidechain").map(fix);
    }
    out
}

/// Track names this track keys off (duck_from, `track:` sidechains).
fn keys_of(tr: &Table, cfg: &Value) -> Vec<String> {
    let mut refs = vec![];
    if let Some(d) = tr
        .get("duck_from")
        .and_then(|d| d.as_str())
        .filter(|d| !d.is_empty())
    {
        refs.push(d.to_string());
    }
    let stage_sc = tr
        .get("stage")
        .and_then(|s| s.as_array())
        .into_iter()
        .flatten()
        .filter_map(|s| s.get("sidechain"));
    for s in [tr.get("sidechain"), cfg.get("sidechain")]
        .into_iter()
        .flatten()
        .chain(stage_sc)
    {
        if let Some(n) = s.as_str().and_then(|s| s.strip_prefix("track:")) {
            refs.push(n.to_string());
        }
    }
    refs
}

/// One track alone, pre-master: the tracks it keys off stay as muted keys.
pub fn stem_config(cfg: &Value, name: &str) -> Value {
    let by: BTreeMap<String, &Table> = tracks_of(cfg)
        .into_iter()
        .filter_map(|t| Some((t.get("name")?.as_str()?.to_string(), t)))
        .collect();
    let (mut keep, mut todo) = (vec![], vec![name.to_string()]);
    while let Some(n) = todo.pop() {
        if keep.contains(&n) || !by.contains_key(&n) {
            continue;
        }
        todo.extend(keys_of(by[&n], cfg));
        keep.push(n);
    }
    let mut out = Table::new();
    for (k, v) in cfg
        .as_table()
        .into_iter()
        .flatten()
        .filter(|(k, _)| !["track", "master", "out"].contains(&k.as_str()))
    {
        out.insert(k.clone(), v.clone());
    }
    let mut tracks = vec![];
    for t in tracks_of(cfg) {
        let tn = t.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if keep.iter().any(|k| k == tn) {
            let mut t = t.clone();
            t.insert("mute".into(), Value::Boolean(tn != name));
            tracks.push(Value::Table(t));
        }
    }
    out.insert("track".into(), Value::Array(tracks));
    Value::Table(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Value {
        toml::from_str(
            r#"
            [[track]]
            name = "gtr"
            input = "gtr.wav"
            gain_db = -3.0
              [[track.stage]]
              plugin = "eq"
              params = { "Mid Freq" = 300.0, "Mid Gain" = 8.0 }
            [[track]]
            name = "kick"
            input = "k.wav"
            [[track]]
            name = "bass"
            input = "b.wav"
            duck_from = "kick"
            [[master]]
            plugin = "limiter"
            params = { Ceiling = -1.0 }
            "#,
        )
        .unwrap()
    }

    #[test]
    fn g_format_matches_python() {
        assert_eq!(
            (g3(0.0), g3(-12.0), g3(0.12345), g3(1234.5), g3(1e-5)),
            (
                "0".into(),
                "-12".into(),
                "0.123".into(),
                "1.23e+03".into(),
                "1e-05".into()
            )
        );
        assert_eq!(
            (g4(300.0), g4(8.0), g4(-3.1415926)),
            ("300".into(), "8".into(), "-3.142".into())
        );
    }

    #[test]
    fn param_rows_keep_names_with_spaces_and_optional_units() {
        let r =
            parse_param_row("  3    Mid Gain        -15.000     15.000      0.000  dB").unwrap();
        assert_eq!(
            (r.id, r.name.as_str(), r.min, r.max, r.default),
            (3, "Mid Gain", -15.0, 15.0, 0.0)
        );
        let r = parse_param_row("  4    Mid Q             0.300      6.000      0.700  ").unwrap();
        assert_eq!((r.name.as_str(), r.default), ("Mid Q", 0.7));
        assert!(
            parse_param_row("  id   name                min        max    default  unit").is_none()
        );
        assert!(parse_param_row("eq — 13 params").is_none());
    }

    #[test]
    fn loss_spec_parses_weights_and_demands_a_reference() {
        let (t, r) = parse_loss("ref:a.wav,masking=2,loudness").unwrap();
        assert_eq!(
            (t, r.as_deref()),
            (
                vec![
                    ("band".into(), 1.0),
                    ("masking".into(), 2.0),
                    ("loudness".into(), 1.0)
                ],
                Some("a.wav")
            )
        );
        assert!(
            parse_loss("loudness").is_err()
                && parse_loss("bogus").is_err()
                && parse_loss("").is_err()
        );
    }

    #[test]
    fn fader_and_stage_targets_read_their_start_and_write_back() {
        let mut c = cfg();
        let mut tables = param_table_cache();
        tables.insert(
            "eq".into(),
            vec![ParamRow {
                id: 3,
                name: "Mid Gain".into(),
                min: -15.0,
                max: 15.0,
                default: 0.0,
            }],
        );
        let chain = Path::new("unused");
        let g = parse_vary("gtr.eq.Mid Gain:-20:20", &mut c, chain, &mut tables).unwrap();
        assert_eq!((g.start, g.lo, g.hi), (8.0, -15.0, 15.0));
        let f = parse_vary("gtr.gain_db:-12:0", &mut c, chain, &mut tables).unwrap();
        assert_eq!(f.start, -3.0);
        let out = apply_values(&c, &[g, f], &[1.5, -6.0]).unwrap();
        assert_eq!(
            out["track"][0]["stage"][0]["params"]["Mid Gain"].as_float(),
            Some(1.5)
        );
        assert_eq!(out["track"][0]["gain_db"].as_float(), Some(-6.0));
        assert_eq!(
            c["track"][0]["stage"][0]["params"]["Mid Gain"].as_float(),
            Some(8.0),
            "the base is untouched"
        );
        assert!(
            parse_vary("nobody.gain_db:-1:1", &mut c, chain, &mut tables)
                .unwrap_err()
                .contains("no track named 'nobody'")
        );
        assert!(
            parse_vary("gtr.eq#2.Mid Gain:-1:1", &mut c, chain, &mut tables)
                .unwrap_err()
                .contains("wanted #2")
        );
    }

    #[test]
    fn a_stem_keeps_the_track_and_mutes_the_tracks_it_ducks_from() {
        let s = stem_config(&cfg(), "bass");
        let names: Vec<(&str, bool)> = s["track"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| (t["name"].as_str().unwrap(), t["mute"].as_bool().unwrap()))
            .collect();
        assert_eq!(names, vec![("kick", true), ("bass", false)]);
        assert!(s.get("master").is_none() && s.get("out").is_none());
    }

    #[test]
    fn inputs_are_pinned_to_the_base_folder_but_track_keys_are_not() {
        let mut c = cfg();
        c["track"][1]
            .as_table_mut()
            .unwrap()
            .insert("sidechain".into(), Value::String("track:gtr".into()));
        let a = absolutise(&c, Path::new("/mix"));
        assert_eq!(a["track"][0]["input"].as_str(), Some("/mix/gtr.wav"));
        assert_eq!(a["track"][1]["sidechain"].as_str(), Some("track:gtr"));
    }
}
