//! SuperDuper Denoise — neural noise / breath suppressor. STUB.
//!
//! Planned: RNNoise (Mozilla, ~85k params) inference via the
//! `nnnoiseless` crate. 480-sample frames @ 48 kHz, ~10 ms latency.
//! Bins-band gain mask multiplied with the STFT magnitude.
//!
//! Next steps:
//!   - Add `nnnoiseless` to deps once we verify it's RT-safe enough.
//!   - 480-sample ring + 480-sample lookahead for the frame.
//!   - Report 10 ms latency via the CLAP `latency` extension so PDC
//!     keeps the bus aligned.
//!   - GUI: noise floor read-out + before/after spectrum strip.
pub const PLANNED: bool = true;
