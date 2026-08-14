//! Reusable CLAP plumbing — every SuperDuper effect/synth plugin used to
//! duplicate this boilerplate verbatim. Centralised here so a bug fix or
//! convention change lands in one place.

use atomic_float::AtomicF32;
use clack_common::events::Pckn;
use clack_common::utils::{ClapId, Cookie};
use clack_extensions::params::{ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter};
use clack_plugin::events::event_types::{
    ParamGestureBeginEvent, ParamGestureEndEvent, ParamValueEvent,
};
use clack_plugin::events::io::OutputEvents;
use clack_plugin::prelude::{ChannelPair, InputEvents};
use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering as SyncOrdering};
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
// Simple CLAP state save/load — for plugins whose entire persistent state
// is just (params + bypass). Each plugin still has to implement
// `PluginStateImpl` (we can't do that from a helper because it needs
// access to the plugin's concrete Shared), but the JSON encode/decode is
// shared so we only get one schema version to manage.
// ---------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize)]
pub struct SimpleState {
    pub version: u32,
    pub params: Vec<f32>,
    pub bypass: bool,
    /// Currently-selected preset index from the factory list. Default 0
    /// for projects saved before this field existed (serde default).
    /// Lets the GUI restore the preset combobox to the right entry on
    /// reopen instead of resetting to "Init".
    #[serde(default)]
    pub active_preset: u32,
}

pub const SIMPLE_STATE_VERSION: u32 = 1;

pub fn save_simple_state(
    params: &[AtomicF32],
    bypass: bool,
    output: &mut clack_common::stream::OutputStream,
) -> Result<(), clack_plugin::plugin::PluginError> {
    save_simple_state_with_preset(params, bypass, 0, output)
}

/// Like `save_simple_state` but also persists the currently-selected
/// preset index. Use this from any plugin that exposes a preset combo
/// so reopening a project doesn't reset the dropdown to "Init".
pub fn save_simple_state_with_preset(
    params: &[AtomicF32],
    bypass: bool,
    active_preset: u32,
    output: &mut clack_common::stream::OutputStream,
) -> Result<(), clack_plugin::plugin::PluginError> {
    let state = SimpleState {
        version: SIMPLE_STATE_VERSION,
        params: params.iter().map(|a| a.load(Ordering::Relaxed)).collect(),
        bypass,
        active_preset,
    };
    serde_json::to_writer(output, &state)
        .map_err(|_| clack_plugin::plugin::PluginError::Message("state JSON write error"))
}

pub fn load_simple_state(
    params: &[AtomicF32],
    input: &mut clack_common::stream::InputStream,
) -> Result<bool, clack_plugin::plugin::PluginError> {
    let (bypass, _) = load_simple_state_with_preset(params, input)?;
    Ok(bypass)
}

/// Like `load_simple_state` but also returns the restored
/// `active_preset` index. Projects saved with the older state format
/// (no preset field) return 0 here.
pub fn load_simple_state_with_preset(
    params: &[AtomicF32],
    input: &mut clack_common::stream::InputStream,
) -> Result<(bool, u32), clack_plugin::plugin::PluginError> {
    let state: SimpleState = serde_json::from_reader(input)
        .map_err(|_| clack_plugin::plugin::PluginError::Message("state JSON read error"))?;
    if state.version != SIMPLE_STATE_VERSION {
        return Err(clack_plugin::plugin::PluginError::Message(
            "state version mismatch",
        ));
    }
    for (i, v) in state.params.iter().enumerate() {
        if let Some(slot) = params.get(i) {
            slot.store(*v, Ordering::Relaxed);
        }
    }
    Ok((state.bypass, state.active_preset))
}

/// Push a `ParamValueEvent` into the host's output queue for every dirty
/// parameter, then clear the bit. Lets GUI-driven changes show up in the
/// host's automation lane — without this, knob moves made in the plugin
/// window are invisible to the DAW.
///
/// Pattern: call once near the top of every `process()` *after* reading
/// the input events but before doing DSP work. Cheap — one swap per
/// param, no allocation.
pub fn emit_dirty_param_events(
    params: &[AtomicF32],
    dirty: &[AtomicBool],
    output: &mut OutputEvents,
) {
    debug_assert_eq!(params.len(), dirty.len());
    for (i, flag) in dirty.iter().enumerate() {
        if flag.swap(false, SyncOrdering::AcqRel) {
            let value = params[i].load(SyncOrdering::Relaxed) as f64;
            let ev = ParamValueEvent::new(
                0,
                ClapId::new(i as u32),
                Pckn::new(0u16, 0u16, 0u16, 0u32),
                value,
                Cookie::empty(),
            );
            let _ = output.try_push(&ev);
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

/// Split a `ChannelPair` while keeping the two directions independent.
///
/// [`split_io`] demands both an input and an output, which is right for a plain
/// effect but wrong for a plugin that is an effect in one mode and an instrument
/// in another (SuperDuper Wind): such a plugin legitimately gets `OutputOnly`
/// buffers and still needs the writable half. This is the one place in the
/// codebase that performs the `InPlace` aliasing split — `split_io` is expressed
/// in terms of it, so the `unsafe` reasoning lives in a single spot.
pub fn split_io_parts<'b>(
    c: ChannelPair<'b, f32>,
) -> (Option<&'b [f32]>, Option<&'b mut [f32]>) {
    match c {
        ChannelPair::InputOutput(i, o) => (Some(i), Some(o)),
        ChannelPair::InPlace(buf) => {
            // SAFETY: the caller reads sample `i` before overwriting index `i` —
            // the same access pattern every audio plugin uses with InPlace.
            let ptr = buf.as_mut_ptr();
            let len = buf.len();
            let read = unsafe { core::slice::from_raw_parts(ptr, len) };
            let write = unsafe { core::slice::from_raw_parts_mut(ptr, len) };
            (Some(read), Some(write))
        }
        ChannelPair::OutputOnly(buf) => (None, Some(buf)),
        ChannelPair::InputOnly(buf) => (Some(buf), None),
    }
}

pub fn split_io<'b>(c: ChannelPair<'b, f32>) -> Option<(&'b [f32], &'b mut [f32])> {
    match split_io_parts(c) {
        (Some(read), Some(write)) => Some((read, write)),
        // No input to process: leave the caller with silence rather than
        // whatever the host's buffer happened to contain.
        (None, Some(write)) => {
            write.fill(0.0);
            None
        }
        _ => None,
    }
}

/// Emit `ParamGestureBeginEvent` / `ParamGestureEndEvent` for every flag
/// the GUI set since the previous block. Pair with `emit_dirty_param_events`
/// — call this immediately after it, so the ordering inside the host's
/// event stream is `Begin → Value → End` even within a single process
/// block (each event is timestamped sample 0 and CLAP preserves insertion
/// order for equal time stamps).
///
/// Why this matters: hosts in *touch* or *latch* automation modes only
/// record while a gesture is open. Without the begin/end markers a knob
/// drag looks like a sequence of disconnected automation points the host
/// can't distinguish from external sample-and-hold modulation.
pub fn emit_gesture_events(
    begin: &[AtomicBool],
    end: &[AtomicBool],
    output: &mut OutputEvents,
) {
    debug_assert_eq!(begin.len(), end.len());
    for (i, flag) in begin.iter().enumerate() {
        if flag.swap(false, SyncOrdering::AcqRel) {
            let ev = ParamGestureBeginEvent::new(0, ClapId::new(i as u32));
            let _ = output.try_push(&ev);
        }
    }
    for (i, flag) in end.iter().enumerate() {
        if flag.swap(false, SyncOrdering::AcqRel) {
            let ev = ParamGestureEndEvent::new(0, ClapId::new(i as u32));
            let _ = output.try_push(&ev);
        }
    }
}

/// Variant of `split_io` for **generators / synthesizers** — anything that
/// has no audio input port and just wants the host's output buffer. The
/// effect-side helper `split_io` returns `None` for `OutputOnly` because
/// effects can't process audio that isn't there; instruments would
/// silently get zeroed buffers if they reused it. Use this from synth
/// plugins (Pad, Ambient) instead.
pub fn output_slice<'b>(c: ChannelPair<'b, f32>) -> Option<&'b mut [f32]> {
    match c {
        ChannelPair::OutputOnly(buf) | ChannelPair::InPlace(buf) => Some(buf),
        ChannelPair::InputOutput(_, buf) => Some(buf),
        ChannelPair::InputOnly(_) => None,
    }
}

// ---------------------------------------------------------------------------
// Preset selector — a host/agent-controllable stepped param that recalls a
// whole preset (wavetable, voice timbre, drum kit, …) without opening the GUI.
//
// Why a helper: every synth duplicates the same recall plumbing, and the RT
// rule is easy to get wrong. The actual recall (`apply_preset`) is plugin-
// specific and ALLOCATES, so it must run on the main thread. Wire it like the
// Wave reference:
//   1. PARAMS: append `ParamDef { name: b"Preset", min: 0, max: N-1, .. }`.
//   2. Shared: keep an `active_preset: AtomicU32` (the last-applied index).
//   3. audio `process()`: if `preset_recall_target(params[P_PRESET], active)`
//      is Some, call `host.shared().request_callback()` — DON'T apply here.
//   4. `on_main_thread()` AND main-thread params `flush()`: if it's Some(idx),
//      run `apply_preset(idx)` (which must store `active_preset = idx`).
//   5. `apply_preset` marks every changed param dirty (see lesson 21d) so the
//      host's LOM/automation — and an agent reading params — sees the recall.
//   6. `value_to_text`/`text_to_value` for the Preset param map index<->name
//      via `preset_value_to_text` / `preset_text_to_value`.
// ---------------------------------------------------------------------------

/// Returns the preset index to recall if the Preset param has moved away from
/// the last-applied index, else `None`. Cheap (one atomic load + compare) and
/// allocation-free, so it's safe to call from the audio thread. The caller
/// decides what to do: the audio thread asks the host for a main-thread
/// callback; the main thread runs the (allocating) `apply_preset`.
#[inline]
pub fn preset_recall_target(preset_param_value: f32, active: &AtomicU32) -> Option<usize> {
    let want = preset_param_value.round();
    if want >= 0.0 && (want as u32) != active.load(Ordering::Relaxed) {
        Some(want as usize)
    } else {
        None
    }
}

/// `value_to_text` body for a Preset selector param: write the preset name for
/// the rounded index. `name_of(i)` returns the i-th preset's name. Returns
/// `None` if the index is out of range (caller falls back to numeric display).
pub fn preset_value_to_text<'a>(
    name_of: impl Fn(usize) -> Option<&'a str>,
    value: f64,
    writer: &mut ParamDisplayWriter<'_>,
) -> Option<core::fmt::Result> {
    use core::fmt::Write;
    let idx = value.round().max(0.0) as usize;
    name_of(idx).map(|name| write!(writer, "{}", name))
}

/// `text_to_value` body for a Preset selector param: resolve a preset name
/// (case-insensitive, trimmed) to its index. `count` presets, `name_of(i)`
/// their names. Returns `None` if nothing matches (caller falls back to the
/// default numeric parse).
pub fn preset_text_to_value<'a>(
    count: usize,
    name_of: impl Fn(usize) -> Option<&'a str>,
    text: &CStr,
) -> Option<f64> {
    let s = text.to_str().ok()?.trim();
    (0..count)
        .find(|&i| name_of(i).is_some_and(|n| n.eq_ignore_ascii_case(s)))
        .map(|i| i as f64)
}
