//! Domain model + repository for user-saved presets.
//!
//! Lives in `synth-core` so any plugin can store and retrieve named
//! presets in `~/.superduper-dsp/<slug>/presets/*.json`, plus an
//! auto-saved "last edited" snapshot at `<slug>/last.json` that becomes
//! the default for fresh plugin instances.
//!
//! ## Domain model
//!
//! - [`PresetName`] — value object, sanitised non-empty `[A-Za-z0-9 _-]{1..=64}`.
//! - [`UserPreset<E>`] — entity, schema-versioned. Generic over a
//!   plugin-specific `extra: E: PresetExtra` payload so e.g. Wave can
//!   carry its drawn `frame_a` curve and Kubyz can carry harmonics +
//!   formants.
//! - [`PresetExtra`] — trait every plugin's extra implements. The
//!   `validate()` method enforces that-which-cannot-be-encoded-in-types
//!   (array lengths, finite floats, positive bandwidths…).
//! - [`PresetError`] — typed errors. Validation failures distinguish
//!   from I/O failures, so the GUI can show specific feedback ("this
//!   preset is from a future format" vs "couldn't write the file").
//!
//! ## Repository
//!
//! [`PresetRepo`] is the I/O boundary. List / load / save by name plus
//! a separate `save_last` / `load_last` for the auto-default. Everything
//! goes through validation — a corrupted `last.json` returns `None`
//! rather than crashing the plugin.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Top-level schema version. Bumped only when the wire format breaks
/// in a non-backwards-compatible way. Plugin-specific extras own their
/// own internal versioning if they need it.
pub const PRESET_FORMAT_VERSION: u32 = 1;

/// Max preset filename length — keeps cross-platform sanity (Windows
/// imposes a 255-byte path limit, with a long install dir we want margin).
pub const PRESET_NAME_MAX: usize = 64;

/// How far the saved param count may drift from the plugin's current count
/// and still load. A build that *appends* a param (e.g. the Preset selector,
/// 37 → 38) must keep loading older presets: the bounded apply loop leaves
/// the new params at their defaults and ignores any extras. A larger gap
/// means the file is for a different plugin and is rejected. Kept smaller
/// than the closest gap between our plugins' param counts so cross-plugin
/// loads still fail.
pub const PARAM_COUNT_TOLERANCE: usize = 4;

#[derive(Debug, Error)]
pub enum PresetError {
    #[error("preset name empty after sanitisation")]
    EmptyName,
    #[error("preset name too long ({0} chars, max {})", PRESET_NAME_MAX)]
    NameTooLong(usize),
    #[error(
        "param count mismatch: expected {expected}, file had {got} \
         (this preset is for a different plugin or build)"
    )]
    ParamCountMismatch { expected: usize, got: usize },
    #[error(
        "unsupported preset format version {0} (this build expects {})",
        PRESET_FORMAT_VERSION
    )]
    UnsupportedVersion(u32),
    #[error("extra payload invalid: {0}")]
    ExtraInvalid(String),
    #[error("filesystem: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------
// PresetName — value object
// ---------------------------------------------------------------------------

/// Sanitised preset name. ASCII alphanumeric + space / dash / underscore;
/// other characters are folded to `_`. Constructed via [`Self::new`] only
/// — the inner `String` is private so consumers can't bypass validation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct PresetName(String);

impl PresetName {
    pub fn new(raw: &str) -> Result<Self, PresetError> {
        let sanitised: String = raw
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_') {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let trimmed = sanitised.trim().to_string();
        if trimmed.is_empty() {
            return Err(PresetError::EmptyName);
        }
        if trimmed.len() > PRESET_NAME_MAX {
            return Err(PresetError::NameTooLong(trimmed.len()));
        }
        Ok(Self(trimmed))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn filename(&self) -> String {
        format!("{}.json", self.0)
    }
}

impl std::fmt::Display for PresetName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ---------------------------------------------------------------------------
// PresetExtra — trait for plugin-specific payloads
// ---------------------------------------------------------------------------

/// Trait every plugin implements on its own `Extra` type to declare the
/// shape and validation rules for its extra preset payload (frame_a curve
/// for Wave, harmonics + formants for Kubyz, etc.).
///
/// Implementations should be cheap: validation runs once on save and
/// once on load, off the audio thread.
pub trait PresetExtra: Serialize + DeserializeOwned + Clone {
    fn validate(&self) -> Result<(), PresetError>;
}

/// `()` is a valid extra payload — for plugins with no extra state
/// beyond params (saturator, EQ, compressor, …) the user preset is just
/// `(name, params)`. Constant-time validate.
impl PresetExtra for () {
    fn validate(&self) -> Result<(), PresetError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// UserPreset — the entity
// ---------------------------------------------------------------------------

/// A user-saved preset. Schema version + sanitised name + a plugin's
/// param vector + plugin-specific extra payload. Round-tripped via JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(serialize = "E: PresetExtra", deserialize = "E: PresetExtra"))]
pub struct UserPreset<E: PresetExtra> {
    pub version: u32,
    pub name: PresetName,
    pub params: Vec<f32>,
    pub extra: E,
}

impl<E: PresetExtra> UserPreset<E> {
    /// Build a new preset from validated components. Will reject the
    /// extra payload immediately if it doesn't satisfy its invariants.
    pub fn new(name: PresetName, params: Vec<f32>, extra: E) -> Result<Self, PresetError> {
        extra.validate()?;
        Ok(Self {
            version: PRESET_FORMAT_VERSION,
            name,
            params,
            extra,
        })
    }

    /// Validate a freshly-loaded preset against the calling plugin's
    /// expected param count. Use this before pushing values into the
    /// plugin's atomic param array so corrupt files can't crash.
    pub fn validate_for(&self, expected_param_count: usize) -> Result<(), PresetError> {
        if self.version != PRESET_FORMAT_VERSION {
            return Err(PresetError::UnsupportedVersion(self.version));
        }
        let lo = expected_param_count.saturating_sub(PARAM_COUNT_TOLERANCE);
        let hi = expected_param_count + PARAM_COUNT_TOLERANCE;
        if !(lo..=hi).contains(&self.params.len()) {
            return Err(PresetError::ParamCountMismatch {
                expected: expected_param_count,
                got: self.params.len(),
            });
        }
        // All param floats must be finite — silent NaN poisoning kills DAWs.
        for (i, &v) in self.params.iter().enumerate() {
            if !v.is_finite() {
                return Err(PresetError::ExtraInvalid(format!(
                    "param #{i} is not finite ({v})"
                )));
            }
        }
        self.extra.validate()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// PresetRepo — filesystem persistence
// ---------------------------------------------------------------------------

/// File-backed repository for user presets and the auto-saved "last
/// edited" snapshot. One instance per plugin slug.
///
/// Layout:
/// ```text
/// ~/.superduper-dsp/<slug>/
///   ├── last.json              ← auto-saved on every edit
///   └── presets/
///       ├── <name>.json        ← user "Save…" actions
///       └── …
/// ```
pub struct PresetRepo<E: PresetExtra> {
    base_dir: PathBuf,
    _phantom: std::marker::PhantomData<E>,
}

impl<E: PresetExtra> PresetRepo<E> {
    pub fn for_plugin(slug: &str) -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        Self::with_base_dir(home.join(".superduper-dsp").join(slug))
    }

    /// Construct against an explicit directory — useful for tests so
    /// each test gets a scratch path instead of fighting over the
    /// global `HOME` env var (which `cargo test`'s default parallel
    /// runner makes a race condition).
    pub fn with_base_dir(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            _phantom: std::marker::PhantomData,
        }
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    fn presets_dir(&self) -> PathBuf {
        self.base_dir.join("presets")
    }

    fn last_path(&self) -> PathBuf {
        self.base_dir.join("last.json")
    }

    /// Sorted list of preset names found on disk. Filenames that don't
    /// pass `PresetName::new` validation are skipped silently.
    pub fn list(&self) -> Vec<PresetName> {
        let dir = self.presets_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|x| x.to_str()) else {
                continue;
            };
            let Ok(name) = PresetName::new(stem) else {
                continue;
            };
            out.push(name);
        }
        out.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        out
    }

    /// Load a named preset and validate it against the plugin's
    /// expected param count. Returns a typed error on any failure.
    pub fn load(
        &self,
        name: &PresetName,
        expected_param_count: usize,
    ) -> Result<UserPreset<E>, PresetError> {
        let path = self.presets_dir().join(name.filename());
        let file = std::fs::File::open(&path)?;
        let preset: UserPreset<E> = serde_json::from_reader(file)?;
        preset.validate_for(expected_param_count)?;
        Ok(preset)
    }

    /// Validate then write to `<base>/presets/<name>.json`. Creates
    /// directories as needed. Returns the final path.
    pub fn save(
        &self,
        preset: &UserPreset<E>,
        expected_param_count: usize,
    ) -> Result<PathBuf, PresetError> {
        preset.validate_for(expected_param_count)?;
        let dir = self.presets_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(preset.name.filename());
        let file = std::fs::File::create(&path)?;
        serde_json::to_writer_pretty(file, preset)?;
        Ok(path)
    }

    /// Write the "last edited" snapshot — overwritten on every save.
    /// Called from the GUI when the user finishes an edit so a fresh
    /// plugin instance can pick up where they left off.
    pub fn save_last(
        &self,
        preset: &UserPreset<E>,
        expected_param_count: usize,
    ) -> Result<(), PresetError> {
        preset.validate_for(expected_param_count)?;
        std::fs::create_dir_all(&self.base_dir)?;
        let file = std::fs::File::create(self.last_path())?;
        serde_json::to_writer(file, preset)?;
        Ok(())
    }

    /// Load the auto-saved "last edited" preset. Returns `None` if the
    /// file is missing, corrupted, or fails validation — caller falls
    /// back to its built-in default.
    pub fn load_last(&self, expected_param_count: usize) -> Option<UserPreset<E>> {
        let file = std::fs::File::open(self.last_path()).ok()?;
        let preset: UserPreset<E> = serde_json::from_reader(file).ok()?;
        preset.validate_for(expected_param_count).ok()?;
        Some(preset)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct TestExtra {
        frame_a: Vec<f32>,
    }

    impl PresetExtra for TestExtra {
        fn validate(&self) -> Result<(), PresetError> {
            if self.frame_a.len() != 16 {
                return Err(PresetError::ExtraInvalid(format!(
                    "frame_a expected 16 samples, got {}",
                    self.frame_a.len()
                )));
            }
            if self.frame_a.iter().any(|s| !s.is_finite()) {
                return Err(PresetError::ExtraInvalid("non-finite sample".into()));
            }
            Ok(())
        }
    }

    #[test]
    fn name_sanitises_punctuation() {
        let n = PresetName::new("Cool Bass!!").unwrap();
        assert_eq!(n.as_str(), "Cool Bass__");
    }

    #[test]
    fn name_rejects_empty() {
        assert!(matches!(
            PresetName::new(""),
            Err(PresetError::EmptyName)
        ));
        // Whitespace-only is also rejected (trims to "").
        assert!(matches!(
            PresetName::new("   "),
            Err(PresetError::EmptyName)
        ));
        // Note: "###" sanitises to "___" — that IS accepted (replacement
        // chars are valid filename characters). Only genuinely empty
        // and whitespace-only names are rejected.
        assert!(PresetName::new("###").is_ok());
    }

    #[test]
    fn name_filename_appends_json() {
        let n = PresetName::new("My Patch").unwrap();
        assert_eq!(n.filename(), "My Patch.json");
    }

    #[test]
    fn preset_validate_catches_param_count_mismatch() {
        let preset: UserPreset<TestExtra> = UserPreset {
            version: PRESET_FORMAT_VERSION,
            name: PresetName::new("a").unwrap(),
            params: vec![0.0, 0.5, 1.0],
            extra: TestExtra { frame_a: vec![0.0; 16] },
        };
        // Within PARAM_COUNT_TOLERANCE (build drift, e.g. an appended param):
        // loads, with the bounded apply leaving new params at their defaults.
        assert!(preset.validate_for(3).is_ok());
        assert!(preset.validate_for(3 + PARAM_COUNT_TOLERANCE).is_ok());
        // A large gap means a different plugin → still rejected.
        let err = preset.validate_for(3 + PARAM_COUNT_TOLERANCE + 1).unwrap_err();
        assert!(matches!(err, PresetError::ParamCountMismatch { got: 3, .. }));
    }

    #[test]
    fn preset_validate_catches_extra_failure() {
        let preset: UserPreset<TestExtra> = UserPreset {
            version: PRESET_FORMAT_VERSION,
            name: PresetName::new("b").unwrap(),
            params: vec![0.0; 3],
            extra: TestExtra { frame_a: vec![0.0; 8] }, // wrong length
        };
        let err = preset.validate_for(3).unwrap_err();
        assert!(matches!(err, PresetError::ExtraInvalid(_)));
    }

    #[test]
    fn preset_validate_catches_nan_param() {
        let preset: UserPreset<TestExtra> = UserPreset {
            version: PRESET_FORMAT_VERSION,
            name: PresetName::new("c").unwrap(),
            params: vec![0.0, f32::NAN, 1.0],
            extra: TestExtra { frame_a: vec![0.0; 16] },
        };
        let err = preset.validate_for(3).unwrap_err();
        assert!(matches!(err, PresetError::ExtraInvalid(_)));
    }

    #[test]
    fn preset_validate_rejects_future_version() {
        let preset: UserPreset<TestExtra> = UserPreset {
            version: 999,
            name: PresetName::new("d").unwrap(),
            params: vec![0.0; 3],
            extra: TestExtra { frame_a: vec![0.0; 16] },
        };
        let err = preset.validate_for(3).unwrap_err();
        assert!(matches!(err, PresetError::UnsupportedVersion(999)));
    }

    #[test]
    fn repo_round_trip() {
        let tmp = std::env::temp_dir().join(format!(
            "sdsp-preset-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::env::set_var("HOME", &tmp);
        let repo: PresetRepo<TestExtra> = PresetRepo::for_plugin("test");
        let preset = UserPreset::new(
            PresetName::new("My Saved").unwrap(),
            vec![0.1, 0.2, 0.3],
            TestExtra { frame_a: vec![0.0; 16] },
        )
        .unwrap();
        repo.save(&preset, 3).unwrap();
        let listed = repo.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].as_str(), "My Saved");
        let loaded = repo.load(&listed[0], 3).unwrap();
        assert_eq!(loaded.params, preset.params);
        assert_eq!(loaded.extra.frame_a.len(), 16);
        // Auto-default
        repo.save_last(&preset, 3).unwrap();
        let last = repo.load_last(3).unwrap();
        assert_eq!(last.name.as_str(), "My Saved");
        // Cleanup
        std::fs::remove_dir_all(&tmp).ok();
    }
}
