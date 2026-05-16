//! Reusable CLAP plumbing — every SuperDuper effect/synth plugin used to
//! duplicate this boilerplate verbatim. Centralised here so a bug fix or
//! convention change lands in one place.

use atomic_float::AtomicF32;
use clack_common::utils::ClapId;
use clack_extensions::params::{ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter};
use clack_plugin::events::event_types::ParamValueEvent;
use clack_plugin::prelude::{ChannelPair, InputEvents};
use std::ffi::CStr;
use std::sync::atomic::Ordering;

// ---------------------------------------------------------------------------
// Parameter description — what each plugin declares in a `const PARAMS: &[ParamDef]`
// ---------------------------------------------------------------------------

#[derive(Copy, Clone)]
pub struct ParamDef {
    pub id: u32,
    pub name: &'static [u8],
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub unit: &'static str,
}

impl ParamDef {
    /// Pre-built `AtomicF32` array initialised to each param's default value.
    /// Use this from `PluginShared::new()` so atomics start at the documented
    /// defaults instead of zero.
    pub fn init_atomics<const N: usize>(table: &'static [ParamDef]) -> [AtomicF32; N] {
        std::array::from_fn(|i| AtomicF32::new(table[i].default as f32))
    }

    /// Lookup a `ParamDef` by CLAP id. Returns None if the host queries
    /// an unknown id (defensive — hosts sometimes scan beyond `count()`).
    pub fn find(table: &'static [ParamDef], id: u32) -> Option<&'static ParamDef> {
        table.iter().find(|p| p.id == id)
    }

    /// Implementation of `PluginParams::get_info`. Pass the plugin's static
    /// table plus the `param_index` the host gave you.
    pub fn write_info(
        table: &'static [ParamDef],
        param_index: u32,
        info: &mut ParamInfoWriter<'_>,
    ) {
        let Some(p) = table.get(param_index as usize) else { return };
        info.set(&ParamInfo {
            id: ClapId::new(p.id),
            flags: ParamInfoFlags::IS_AUTOMATABLE,
            cookie: Default::default(),
            name: p.name,
            module: b"",
            min_value: p.min,
            max_value: p.max,
            default_value: p.default,
        });
    }

    /// Implementation of `PluginParams::value_to_text`. Formats `value` to
    /// 2 decimal places, optionally suffixed with the param's unit.
    pub fn write_display(
        table: &'static [ParamDef],
        param_id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter<'_>,
    ) -> core::fmt::Result {
        use core::fmt::Write;
        let Some(p) = Self::find(table, param_id.get()) else { return Ok(()) };
        if p.unit.is_empty() {
            write!(writer, "{:.2}", value)
        } else {
            write!(writer, "{:.2} {}", value, p.unit)
        }
    }

    /// Implementation of `PluginParams::text_to_value`. Strips the unit
    /// suffix (if any), parses to f64, clamps to the documented range.
    pub fn parse_text(
        table: &'static [ParamDef],
        param_id: ClapId,
        text: &CStr,
    ) -> Option<f64> {
        let p = Self::find(table, param_id.get())?;
        let s = text.to_str().ok()?.trim();
        let s = if !p.unit.is_empty() {
            s.strip_suffix(p.unit).unwrap_or(s).trim()
        } else {
            s
        };
        s.parse::<f64>().ok().map(|v| v.clamp(p.min, p.max))
    }
}

// ---------------------------------------------------------------------------
// Param event dispatch — every CLAP plugin needs to read ParamValueEvent
// from the input event stream and store it into atomics.
//
// IMPORTANT: `pv.param_id()` returns `Option<ClapId>`, NOT `ClapId`. Trying
// to compare it directly to a `ClapId` would silently always be false
// (via a blanket `PartialEq` impl). Always destructure.
// ---------------------------------------------------------------------------

pub fn apply_param_events(params: &[AtomicF32], events: &InputEvents) {
    for event in events {
        let Some(pv) = event.as_event::<ParamValueEvent>() else { continue };
        let Some(id) = pv.param_id() else { continue };
        let i = id.get() as usize;
        if let Some(slot) = params.get(i) {
            slot.store(pv.value() as f32, Ordering::Relaxed);
        }
    }
}

// ---------------------------------------------------------------------------
// ChannelPair → (read, write) splitter.
//
// Handles every CLAP buffer mode (InputOutput / InPlace / OutputOnly /
// InputOnly). For InPlace we hand back two slices into the same buffer —
// safe because the caller reads index `i` before writing to index `i`.
// ---------------------------------------------------------------------------

pub fn split_io<'b>(c: ChannelPair<'b, f32>) -> Option<(&'b [f32], &'b mut [f32])> {
    match c {
        ChannelPair::InputOutput(i, o) => Some((i, o)),
        ChannelPair::InPlace(buf) => {
            // SAFETY: caller reads sample `i` before overwriting index `i` —
            // the same access pattern every audio plugin uses with InPlace.
            let ptr = buf.as_mut_ptr();
            let len = buf.len();
            let read = unsafe { core::slice::from_raw_parts(ptr, len) };
            let write = unsafe { core::slice::from_raw_parts_mut(ptr, len) };
            Some((read, write))
        }
        ChannelPair::OutputOnly(buf) => {
            buf.fill(0.0);
            None
        }
        ChannelPair::InputOnly(_) => None,
    }
}
