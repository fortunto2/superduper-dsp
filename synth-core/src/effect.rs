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
//! fn process(&mut self, _: Process, mut audio: Audio, events: Events) -> Result<ProcessStatus, PluginError> {
//!     let p = self.settings();            // one snapshot of the atomics per block
//!     effect::stereo_block(self.shared, &mut audio, &mut events, |l_in, l_out, r| {
//!         self.dsp.run(&p, l_in, l_out, r);
//!     })
//! }
//! ```

use atomic_float::AtomicF32;
use clack_plugin::prelude::*;
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

/// One block of a stereo (or mono) effect. Returns `Continue`.
///
/// Bypassed: input copied to output, DSP not called (state freezes, as every
/// plugin here always did), the dry signal fed to the scope. Otherwise the
/// closure runs once per port and the scope gets the output. Mono ports hand
/// the closure `None` for the right channel.
pub fn stereo_block<S, F>(shared: &S, audio: &mut Audio, events: &mut Events, mut dsp: F) -> Result<ProcessStatus, PluginError>
where
    S: SharedHost,
    F: FnMut(&[f32], &mut [f32], Option<(&[f32], &mut [f32])>),
{
    let _denormals = superduper_dsp_sdk::denormals::Guard::new();
    apply_param_events(shared.params(), events.input);
    emit_dirty_param_events(shared.params(), shared.dirty_params(), events.output);
    emit_gesture_events(shared.gesture_begin(), shared.gesture_end(), events.output);
    let bypassed = shared.bypass().load(std::sync::atomic::Ordering::Relaxed);
    let scope = shared.scope();

    for mut port_pair in audio {
        let Some(channel_pairs) = port_pair.channels()?.into_f32() else { continue };
        let mut chans = channel_pairs.into_iter();
        let Some((l_in, l_out)) = chans.next().and_then(split_io) else { continue };
        let mut r = chans.next().and_then(split_io);
        if bypassed {
            l_out.copy_from_slice(l_in);
            match r {
                Some((r_in, r_out)) => {
                    r_out.copy_from_slice(r_in);
                    for (a, b) in l_out.iter().zip(r_out.iter()) { scope.push(0.5 * (a + b)); }
                }
                None => for a in l_out.iter() { scope.push(*a); },
            }
            continue;
        }
        dsp(l_in, l_out, r.as_mut().map(|(ri, ro)| (*ri, &mut **ro)));
        match r {
            Some((_, r_out)) => for (a, b) in l_out.iter().zip(r_out.iter()) { scope.push(0.5 * (a + b)); },
            None => for a in l_out.iter() { scope.push(*a); },
        }
    }
    Ok(ProcessStatus::Continue)
}
