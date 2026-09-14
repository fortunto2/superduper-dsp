//! A connectome as a sparse graph, and the leaky integrate-and-fire step over it.
//!
//! Scope on purpose: this crate loads an edge list and integrates. It has no opinion about
//! audio, no opinion about a body, and no platform dependencies, because both products
//! that use it — an iOS app and a CLAP plugin — need the same arithmetic and nothing else.
//!
//! What it refuses to do is claim success quietly. [`Receipt`] exists so a run reports what
//! it actually computed (neurons, edges, steps, spikes) rather than returning `Ok(())`;
//! a simulation that emits zero spikes and a simulation that never ran look identical from
//! the outside, and that is the failure this crate is written to make impossible.

pub mod graph;
pub mod lif;
pub mod receipt;

pub use graph::{Graph, GraphError};
pub use lif::{Lif, LifParams};
pub use receipt::Receipt;
