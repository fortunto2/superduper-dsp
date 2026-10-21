//! SFZ text → flat list of regions with every inherited opcode resolved.
//!
//! Two passes. The preprocessor expands `#include` (paths relative to the
//! root `.sfz`, as ARIA/sfizz do) and `#define` **in stream order** — a
//! define can be redefined between groups (Salamander redefines `$VEL` per
//! velocity layer), so substitution happens token by token as the stream is
//! read, never as a global search-and-replace. The assembler then walks the
//! tokens, tracking `<control>` → `<global>` → `<master>` → `<group>` →
//! `<region>` inheritance.
//!
//! File access goes through a closure so the parser is testable without a
//! filesystem and stays usable from iOS.

use std::collections::HashMap;

/// One opcode as written: `key=value`, both already `$define`-substituted.
pub type Opcodes = Vec<(String, String)>;

/// Parsed file: the regions with inherited opcodes merged (outer first, so a
/// later entry overrides an earlier one), plus the `<control>` and `<curve>`
/// blocks which are not inherited.
#[derive(Debug, Default, Clone)]
pub struct SfzFile {
    pub control: Opcodes,
    pub curves: Vec<Opcodes>,
    pub regions: Vec<Opcodes>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ParseError {
    #[error("#include \"{0}\" could not be read")]
    Include(String),
    #[error("#include nesting deeper than 32 (a file includes itself?)")]
    IncludeDepth,
}

/// Preprocess + assemble. `read(path)` returns the text of an included file;
/// `path` is exactly what the `#include` named.
pub fn parse_sfz(
    root_text: &str,
    read: &mut dyn FnMut(&str) -> Option<String>,
) -> Result<SfzFile, ParseError> {
    let mut defines: Vec<(String, String)> = Vec::new();
    let mut tokens = Vec::new();
    preprocess(root_text, read, &mut defines, &mut tokens, 0)?;
    Ok(assemble(&tokens))
}

fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' { i += 1; }
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) { i += 1; }
            i += 2;
            out.push(' ');
        } else {
            // Push the whole UTF-8 char starting here.
            let ch = text[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Raw tokens: `<header>` split off even when glued to an opcode, and
/// `"quoted strings"` kept whole (only `#include` uses them). Line breaks are
/// kept as a `\n` token — a value with spaces (`sample=My File.wav`) may run
/// to the next opcode but never across a line.
fn raw_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut chars = line.chars().peekable();
        let mut cur = String::new();
        while let Some(c) = chars.next() {
            match c {
                '<' => {
                    if !cur.is_empty() { out.push(std::mem::take(&mut cur)); }
                    let mut h = String::from("<");
                    for c2 in chars.by_ref() {
                        h.push(c2);
                        if c2 == '>' { break; }
                    }
                    out.push(h);
                }
                '"' => {
                    if !cur.is_empty() { out.push(std::mem::take(&mut cur)); }
                    let mut q = String::from("\"");
                    for c2 in chars.by_ref() {
                        if c2 == '"' { break; }
                        q.push(c2);
                    }
                    out.push(q);
                }
                c if c.is_whitespace() => {
                    if !cur.is_empty() { out.push(std::mem::take(&mut cur)); }
                }
                c => cur.push(c),
            }
        }
        if !cur.is_empty() { out.push(cur); }
        out.push("\n".into());
    }
    out
}

/// Replace every `$NAME` with its current value, longest name first at each
/// position (`$VEL` must not eat the start of `$VELTRACK`).
fn substitute(tok: &str, defines: &[(String, String)]) -> String {
    if !tok.contains('$') { return tok.to_string(); }
    let mut out = String::with_capacity(tok.len());
    let mut rest = tok;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];
        let best = defines
            .iter()
            .filter(|(name, _)| tail.starts_with(name.as_str()))
            .max_by_key(|(name, _)| name.len());
        match best {
            Some((name, val)) => {
                out.push_str(val);
                rest = &tail[name.len()..];
            }
            None => {
                out.push('$');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn preprocess(
    text: &str,
    read: &mut dyn FnMut(&str) -> Option<String>,
    defines: &mut Vec<(String, String)>,
    out: &mut Vec<String>,
    depth: usize,
) -> Result<(), ParseError> {
    if depth > 32 { return Err(ParseError::IncludeDepth); }
    let toks = raw_tokens(&strip_comments(text));
    let mut i = 0;
    while i < toks.len() {
        let t = &toks[i];
        if t == "#define" {
            if let (Some(name), Some(val)) = (toks.get(i + 1), toks.get(i + 2)) {
                let val = substitute(val, defines);
                match defines.iter_mut().find(|(n, _)| n == name) {
                    Some(slot) => slot.1 = val,
                    None => defines.push((name.clone(), val)),
                }
            }
            i += 3;
            continue;
        }
        if t == "#include" {
            if let Some(q) = toks.get(i + 1) {
                let path = substitute(q.trim_start_matches('"'), defines);
                let inc = read(&path).ok_or(ParseError::Include(path.clone()))?;
                preprocess(&inc, read, defines, out, depth + 1)?;
            }
            i += 2;
            continue;
        }
        out.push(substitute(t, defines));
        i += 1;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq)]
enum Section { None, Control, Global, Master, Group, Region, Curve, Other }

fn assemble(tokens: &[String]) -> SfzFile {
    let mut file = SfzFile::default();
    let (mut global, mut master, mut group, mut region) =
        (Opcodes::new(), Opcodes::new(), Opcodes::new(), Opcodes::new());
    let mut curve = Opcodes::new();
    let mut section = Section::None;
    // Whether the last opcode may still absorb a following bare word (a
    // value with spaces). Reset on every line break and header.
    let mut open_value = false;

    let flush_region = |file: &mut SfzFile, g: &Opcodes, m: &Opcodes, gr: &Opcodes, r: &mut Opcodes| {
        let mut merged = Opcodes::with_capacity(g.len() + m.len() + gr.len() + r.len());
        merged.extend(g.iter().cloned());
        merged.extend(m.iter().cloned());
        merged.extend(gr.iter().cloned());
        merged.append(r);
        file.regions.push(merged);
    };

    for tok in tokens {
        if tok == "\n" { open_value = false; continue; }
        if tok.starts_with('<') && tok.ends_with('>') {
            if section == Section::Region { flush_region(&mut file, &global, &master, &group, &mut region); }
            if section == Section::Curve { file.curves.push(std::mem::take(&mut curve)); }
            open_value = false;
            section = match tok.as_str() {
                "<control>" => Section::Control,
                "<global>" => { global.clear(); master.clear(); group.clear(); Section::Global }
                "<master>" => { master.clear(); group.clear(); Section::Master }
                "<group>" => { group.clear(); Section::Group }
                "<region>" => Section::Region,
                "<curve>" => Section::Curve,
                _ => Section::Other,
            };
            continue;
        }
        let target = match section {
            Section::Control => &mut file.control,
            Section::Global => &mut global,
            Section::Master => &mut master,
            Section::Group => &mut group,
            Section::Region => &mut region,
            Section::Curve => &mut curve,
            Section::None | Section::Other => continue,
        };
        match tok.split_once('=') {
            Some((k, v)) if !k.is_empty() => {
                target.push((k.to_string(), v.to_string()));
                open_value = true;
            }
            _ if open_value => {
                if let Some(last) = target.last_mut() {
                    last.1.push(' ');
                    last.1.push_str(tok);
                }
            }
            _ => {}
        }
    }
    if section == Section::Region { flush_region(&mut file, &global, &master, &group, &mut region); }
    if section == Section::Curve { file.curves.push(curve); }
    file
}

/// Last value of `key` in an opcode list (later = more specific wins).
pub fn get<'a>(ops: &'a Opcodes, key: &str) -> Option<&'a str> {
    ops.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// Collapse an opcode list into a map, later entries winning.
pub fn to_map(ops: &Opcodes) -> HashMap<&str, &str> {
    ops.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

/// MIDI key from a number or a note name. SFZ convention: `c4` = 60, so
/// `c-1` = 0; sharps `#`, flats `b`, case-insensitive.
pub fn parse_key(s: &str) -> Option<i32> {
    let s = s.trim();
    if let Ok(n) = s.parse::<i32>() { return Some(n); }
    let lower = s.to_ascii_lowercase();
    let mut chars = lower.chars();
    let base = match chars.next()? {
        'c' => 0, 'd' => 2, 'e' => 4, 'f' => 5, 'g' => 7, 'a' => 9, 'b' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let (acc, oct) = if let Some(r) = rest.strip_prefix('#') { (1, r) }
        else if let Some(r) = rest.strip_prefix('b') { (-1, r) }
        else { (0, rest.as_str()) };
    let octave: i32 = oct.parse().ok()?;
    Some((octave + 1) * 12 + base + acc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str, files: &[(&str, &str)]) -> SfzFile {
        let mut read = |p: &str| files.iter().find(|(n, _)| *n == p).map(|(_, t)| t.to_string());
        parse_sfz(text, &mut read).expect("parse")
    }

    #[test]
    fn inheritance_global_master_group_region() {
        let f = parse(
            "<global> volume=-3 <master> tune=5 <group> lovel=10 <region> sample=a.wav <region> sample=b.wav tune=7",
            &[],
        );
        assert_eq!(f.regions.len(), 2);
        assert_eq!(get(&f.regions[0], "volume"), Some("-3"));
        assert_eq!(get(&f.regions[0], "tune"), Some("5"));
        assert_eq!(get(&f.regions[1], "tune"), Some("7"), "region overrides master");
        assert_eq!(get(&f.regions[1], "lovel"), Some("10"));
    }

    #[test]
    fn new_global_resets_master_and_group() {
        let f = parse("<master> tune=5 <group> lovel=10 <global> <region> sample=a.wav", &[]);
        assert_eq!(get(&f.regions[0], "tune"), None);
        assert_eq!(get(&f.regions[0], "lovel"), None);
    }

    #[test]
    fn defines_are_redefined_in_stream_order_longest_name_first() {
        let f = parse(
            "#define $VEL v1\n#define $VELTRACK 99\n<group> amp_veltrack_oncc$VELTRACK=-100\n<region> sample=A0$VEL.flac\n#define $VEL v2\n<region> sample=A0$VEL.flac",
            &[],
        );
        assert_eq!(get(&f.regions[0], "sample"), Some("A0v1.flac"));
        assert_eq!(get(&f.regions[1], "sample"), Some("A0v2.flac"));
        assert_eq!(get(&f.regions[0], "amp_veltrack_oncc99"), Some("-100"));
    }

    #[test]
    fn include_mid_line_like_salamander_notes_txt() {
        let f = parse(
            "<group> #include \"vel.txt\" lovel=1 hivel=26 #include \"region.txt\"",
            &[("vel.txt", "#define $VEL v1"), ("region.txt", "<region> lokey=21 hikey=22 sample=A0$VEL.flac")],
        );
        assert_eq!(f.regions.len(), 1);
        assert_eq!(get(&f.regions[0], "sample"), Some("A0v1.flac"));
        assert_eq!(get(&f.regions[0], "hivel"), Some("26"));
    }

    #[test]
    fn missing_include_is_an_error_not_silence() {
        let mut read = |_: &str| None;
        assert_eq!(
            parse_sfz("#include \"nope.txt\"", &mut read).unwrap_err(),
            ParseError::Include("nope.txt".into())
        );
    }

    #[test]
    fn sample_with_spaces_and_comments() {
        let f = parse(
            "<region> sample=Grand Piano C4.wav lokey=60 // comment sample=x\n/* block\n<region> */ <region> sample=b.wav",
            &[],
        );
        assert_eq!(f.regions.len(), 2);
        assert_eq!(get(&f.regions[0], "sample"), Some("Grand Piano C4.wav"));
        assert_eq!(get(&f.regions[0], "lokey"), Some("60"));
    }

    #[test]
    fn control_and_curve_blocks() {
        let f = parse("<control> default_path=Samples/ set_cc7=100 <curve> curve_index=7 v000=0 v127=1 <region> sample=a.wav", &[]);
        assert_eq!(get(&f.control, "default_path"), Some("Samples/"));
        assert_eq!(f.curves.len(), 1);
        assert_eq!(get(&f.curves[0], "curve_index"), Some("7"));
        assert_eq!(get(&f.regions[0], "default_path"), None, "control is not inherited");
    }

    #[test]
    fn note_names() {
        assert_eq!(parse_key("c4"), Some(60));
        assert_eq!(parse_key("C0"), Some(12));
        assert_eq!(parse_key("C#0"), Some(13));
        assert_eq!(parse_key("a0"), Some(21));
        assert_eq!(parse_key("Bb3"), Some(58));
        assert_eq!(parse_key("-1"), Some(-1));
        assert_eq!(parse_key("x"), None);
    }
}
