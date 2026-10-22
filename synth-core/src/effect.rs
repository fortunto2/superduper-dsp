//! The front half of every effect's `process()`, once.
//!
//! Each of the 31 effects repeated the same forty lines: denormal guard,
//! parameter events in, dirty / gesture events out, walk the ports, split
//! each channel into read and write halves, and a bypass path that copied
//! dry and returned — *before* the bus scope was fed, so a bypassed plugin
//! read as silence on the bus (measured 2026-10-21: `prefer: Morph` lost the
//! whole 808). `stereo_block` does all of it, and the scope always sees
//! what leaves the plugin, bypassed or not. The plugin supplies only its DSP
//! as a closure over `(l_in, l_out, Option<(r_in, r_out)>)`.
//!
//! ```ignore
//! fn process(&mut self, process: Process, mut audio: Audio, mut events: Events) -> Result<ProcessStatus, PluginError> {
//!     let shared = self.shared;
//!     let _denormals = effect::begin_events(&**shared, &process, &mut events);   // song time, params in, dirty/gesture out, FTZ on
//!     let p = self.settings();                        // one snapshot of the atomics per block
//!     effect::run_ports(&**shared, &mut audio, |l_in, l_out, r| {
//!         self.dsp.run(&p, l_in, l_out, r);
//!     })
//! }
//! ```

use atomic_float::AtomicF32;
use clack_plugin::prelude::*;
use clack_plugin::process::audio::PortPair;
use std::sync::atomic::AtomicBool;
use superduper_dsp_sdk::clap_helpers::{
    apply_param_events, emit_dirty_param_events, emit_gesture_events, split_io,
};

use crate::gui::LiveScope;

/// What the block runner needs from a plugin's shared state. Every plugin
/// already has these fields under these names; `impl_shared_host!` wires them.
pub trait SharedHost {
    fn params(&self) -> &[AtomicF32];
    fn dirty_params(&self) -> &[AtomicBool];
    fn gesture_begin(&self) -> &[AtomicBool];
    fn gesture_end(&self) -> &[AtomicBool];
    fn bypass(&self) -> &AtomicBool;
    fn scope(&self) -> &LiveScope;
}

/// `impl SharedHost` for a struct with the conventional fields.
#[macro_export]
macro_rules! impl_shared_host {
    ($ty:ty) => {
        impl $crate::effect::SharedHost for $ty {
            fn params(&self) -> &[::atomic_float::AtomicF32] { &self.params }
            fn dirty_params(&self) -> &[::std::sync::atomic::AtomicBool] { &self.dirty_params }
            fn gesture_begin(&self) -> &[::std::sync::atomic::AtomicBool] { &self.gesture_begin }
            fn gesture_end(&self) -> &[::std::sync::atomic::AtomicBool] { &self.gesture_end }
            fn bypass(&self) -> &::std::sync::atomic::AtomicBool { &self.bypass }
            fn scope(&self) -> &$crate::gui::LiveScope { &self.scope }
        }
    };
}

/// One block of a stereo (or mono) effect: [`begin_events`] then
/// [`run_ports`]. For a plugin whose DSP reads the parameters itself (EQ).
pub fn stereo_block<S, F>(shared: &S, process: &Process, audio: &mut Audio, events: &mut Events, dsp: F) -> Result<ProcessStatus, PluginError>
where
    S: SharedHost,
    F: FnMut(&[f32], &mut [f32], Option<(&[f32], &mut [f32])>),
{
    let _denormals = begin_events(shared, process, events);
    run_ports(shared, audio, dsp)
}

/// The event half: the block's song time to the bus key
/// (`LiveScope::begin_block`), parameter events in, dirty / gesture events
/// out. Call it
/// **before** snapshotting parameters for the block, or the block runs on
/// last block's values (one block of automation lag; chorus/filter/midside
/// had it for an hour on 2026-10-21).
///
/// Returns the denormal guard for the whole `process()` — hold it
/// (`let _denormals = …`). The runners flush denormals too, but work a
/// plugin does *between* this and the runner (the Looper applying a
/// record / overdub command to a buffer of subnormals) ran without FTZ and
/// clap-validator measured it 26–65× slower on subnormal input (2026-10-21).
#[must_use = "hold the guard for the whole process(): let _denormals = begin_events(..)"]
pub fn begin_events<S: SharedHost>(shared: &S, process: &Process, events: &mut Events) -> superduper_dsp_sdk::denormals::Guard {
    let guard = superduper_dsp_sdk::denormals::Guard::new();
    shared.scope().begin_block(process.transport);
    apply_param_events(shared.params(), events.input);
    emit_dirty_param_events(shared.params(), shared.dirty_params(), events.output);
    emit_gesture_events(shared.gesture_begin(), shared.gesture_end(), events.output);
    guard
}

/// The audio half, every port pair. Denormals flushed for the block.
/// Bypassed: input copied to output, DSP not called (state freezes, as every
/// plugin here always did), the dry signal fed to the scope. Otherwise the
/// closure runs once per port and the scope gets the output. Mono ports hand
/// the closure `None` for the right channel.
pub fn run_ports<S, F>(shared: &S, audio: &mut Audio, mut dsp: F) -> Result<ProcessStatus, PluginError>
where
    S: SharedHost,
    F: FnMut(&[f32], &mut [f32], Option<(&[f32], &mut [f32])>),
{
    run_ports_with(shared, audio, |bypassed, l_in, l_out, r| {
        if bypassed { copy_dry(l_in, l_out, r) } else { dsp(l_in, l_out, r) }
    })
}

/// [`run_ports`] where the closure sees the bypass flag and writes the
/// bypassed output itself — for a plugin whose bypass must keep its latency
/// delay running (Saturator, Double, Vocoder: bypass must not move PDC).
/// The scope is still fed from whatever the closure wrote.
pub fn run_ports_with<S, F>(shared: &S, audio: &mut Audio, mut dsp: F) -> Result<ProcessStatus, PluginError>
where
    S: SharedHost,
    F: FnMut(bool, &[f32], &mut [f32], Option<(&[f32], &mut [f32])>),
{
    let _denormals = superduper_dsp_sdk::denormals::Guard::new();
    let bypassed = shared.bypass().load(std::sync::atomic::Ordering::Relaxed);
    let scope = shared.scope();
    for port_pair in audio {
        one_port(scope, bypassed, port_pair, &mut dsp)?;
    }
    scope.end_block();
    Ok(ProcessStatus::Continue)
}

/// [`run_ports`] for a plugin with a side-chain input: port 0 only. The key
/// port was read before (`KeyCapture::capture`), so walking every pair would
/// hand the closure the key as if it were audio to process.
pub fn run_main_port<S, F>(shared: &S, audio: &mut Audio, mut dsp: F) -> Result<ProcessStatus, PluginError>
where
    S: SharedHost,
    F: FnMut(&[f32], &mut [f32], Option<(&[f32], &mut [f32])>),
{
    run_main_port_with(shared, audio, |bypassed, l_in, l_out, r| {
        if bypassed { copy_dry(l_in, l_out, r) } else { dsp(l_in, l_out, r) }
    })
}

/// [`run_main_port`] with the bypass flag handed to the closure (see
/// [`run_ports_with`]).
pub fn run_main_port_with<S, F>(shared: &S, audio: &mut Audio, mut dsp: F) -> Result<ProcessStatus, PluginError>
where
    S: SharedHost,
    F: FnMut(bool, &[f32], &mut [f32], Option<(&[f32], &mut [f32])>),
{
    let _denormals = superduper_dsp_sdk::denormals::Guard::new();
    let bypassed = shared.bypass().load(std::sync::atomic::Ordering::Relaxed);
    if let Some(port_pair) = audio.port_pair(0) {
        one_port(shared.scope(), bypassed, port_pair, &mut dsp)?;
    }
    shared.scope().end_block();
    Ok(ProcessStatus::Continue)
}

fn copy_dry(l_in: &[f32], l_out: &mut [f32], r: Option<(&[f32], &mut [f32])>) {
    l_out.copy_from_slice(l_in);
    if let Some((r_in, r_out)) = r { r_out.copy_from_slice(r_in); }
}

fn one_port<F>(scope: &LiveScope, bypassed: bool, mut port_pair: PortPair<'_>, dsp: &mut F) -> Result<(), PluginError>
where
    F: FnMut(bool, &[f32], &mut [f32], Option<(&[f32], &mut [f32])>),
{
    let Some(channel_pairs) = port_pair.channels()?.into_f32() else { return Ok(()) };
    let mut chans = channel_pairs.into_iter();
    let Some((l_in, l_out)) = chans.next().and_then(split_io) else { return Ok(()) };
    let mut r = chans.next().and_then(split_io);
    dsp(bypassed, l_in, l_out, r.as_mut().map(|(ri, ro)| (*ri, &mut **ro)));
    match r {
        Some((_, r_out)) => for (a, b) in l_out.iter().zip(r_out.iter()) { scope.push(0.5 * (a + b)); },
        None => for a in l_out.iter() { scope.push(*a); },
    }
    Ok(())
}
