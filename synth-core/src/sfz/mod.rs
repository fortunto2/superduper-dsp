//! SFZ sampler engine — the open multisample format (sfzformat.com) behind
//! SuperDuper Piano, and later Sampler's SFZ / Kit modes.
//!
//! - `parse`  — text → regions with inherited opcodes (`#define`/`#include`
//!   expanded in stream order).
//! - `region` — opcodes → typed `Region`s + curves; unknown opcodes listed.
//! - `engine` — voices, pedal, release triggers, keyswitches; RT-safe.
//! - `load`   — files → decoded `Instrument` + pack scan (feature `sfz-load`,
//!   it pulls in symphonia and touches the filesystem).

pub mod engine;
pub mod parse;
pub mod region;
#[cfg(feature = "sfz-load")]
pub mod load;

pub use engine::{Controls, Engine, Instrument, SampleBuf, DEFAULT_VOICES};
pub use region::{InstrumentDef, Region};
