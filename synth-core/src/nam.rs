//! Neural Amp Modeler — WaveNet inference, ported from
//! [`NeuralAmpModelerCore`](https://github.com/sdatkinson/NeuralAmpModelerCore)
//! (`NAM/wavenet/model.cpp`).
//!
//! Architecture (matches NAM 0.5.x):
//!
//! ```text
//! WaveNet:
//!   for each LayerArray (stack):
//!     _rechannel: Conv1x1(input_size → channels, no bias)
//!     for each Layer (dilation in stack):
//!       _conv:        Conv1D(channels → bottleneck or 2×bottleneck if gated,
//!                            kernel_size, dilation, bias)
//!       _input_mixin: Conv1x1(condition_size → bottleneck, no bias)
//!       _layer1x1:    Conv1x1(bottleneck → channels, bias)  if layer1x1.active
//!       residual = input + layer1x1(activation(conv(input) + input_mixin(cond)))
//!       head_input[layer] = activation(conv(input) + input_mixin(cond))
//!     _head_rechannel: Conv1D(bottleneck → head_size, head_kernel_size,
//!                             dilation=1, bias=head_bias)
//!   head_scale: f32
//! ```
//!
//! Stack outputs feed each other (stack[k+1].input = stack[k].layer_outputs).
//! Head outputs of stack[k+1] are added on top of stack[k]'s
//! head_rechannel result — the final stack's head output × head_scale
//! is the single audio sample we emit.
//!
//! All math is sample-by-sample, RT-safe (no heap, no locks). Per-layer
//! convolutions are dot products into a per-layer history ring buffer.
//!
//! Reference weight ordering ([`NAM/conv1d.cpp:9`](https://github.com/sdatkinson/NeuralAmpModelerCore/blob/main/NAM/conv1d.cpp#L9)):
//! ```text
//! for o in [0..out_channels):
//!   for i in [0..in_channels):
//!     for k in [0..kernel_size):
//!       weight[k][o][i] = *it++
//! for b in [0..bias_size):
//!   bias[b] = *it++
//! ```
//!
//! Per-Layer in NAM `Layer::set_weights_` ([`model.cpp:135`](https://github.com/sdatkinson/NeuralAmpModelerCore/blob/main/NAM/wavenet/model.cpp#L135)):
//! 1. `_conv` (Conv1D, with bias)
//! 2. `_input_mixin` (Conv1x1, no bias)
//! 3. `_layer1x1` (Conv1x1, with bias) if active
//!
//! Per-LayerArray in NAM `LayerArray::set_weights_` ([`model.cpp:525`](https://github.com/sdatkinson/NeuralAmpModelerCore/blob/main/NAM/wavenet/model.cpp#L525)):
//! 1. `_rechannel` (Conv1x1, no bias)
//! 2. each layer in order
//! 3. `_head_rechannel` (Conv1D, with bias iff `head_bias=true`)
//!
//! At the WaveNet level ([`model.cpp:623`](https://github.com/sdatkinson/NeuralAmpModelerCore/blob/main/NAM/wavenet/model.cpp#L623)):
//! 1. each layer_array
//! 2. (post_stack_head — not supported here)
//! 3. `head_scale` (single float)

use serde::Deserialize;

/// How the per-layer activation output is constructed. Maps to NAM's
/// `GatingMode` enum (NAM/wavenet/params.h).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum GatingMode {
    /// `y = activation(scratch)` — no gating, no second channel split.
    None,
    /// `y = activation(A) * secondary_activation(B)` — DeepMind WaveNet
    /// gate. Conv outputs `2 × bottleneck`; first half is A, second is B.
    Gated,
    /// `alpha = secondary_activation(B); y = alpha * activation(A) + (1 - alpha) * A`
    /// — soft blend between activated and pre-activation paths.
    Blended,
}

impl GatingMode {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "gated" => Some(Self::Gated),
            "blended" => Some(Self::Blended),
            _ => None,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum Activation {
    Tanh,
    Relu,
    Sigmoid,
    Hardtanh,
    Softsign,
}

impl Activation {
    fn parse(name: &str) -> Result<Self, NamError> {
        match name {
            "Tanh" => Ok(Self::Tanh),
            "ReLU" | "Relu" => Ok(Self::Relu),
            "Sigmoid" => Ok(Self::Sigmoid),
            "Hardtanh" => Ok(Self::Hardtanh),
            "Softsign" => Ok(Self::Softsign),
            other => Err(NamError::UnknownActivation(other.into())),
        }
    }
    #[inline]
    fn apply(self, x: f32) -> f32 {
        match self {
            Self::Tanh => x.tanh(),
            Self::Relu => x.max(0.0),
            Self::Sigmoid => 1.0 / (1.0 + (-x).exp()),
            Self::Hardtanh => x.clamp(-1.0, 1.0),
            // x / (1 + |x|). Cheap, no exp/tanh — gives a softer knee
            // than tanh and matches NAM's "Softsign" activation choice.
            Self::Softsign => x / (1.0 + x.abs()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NamError {
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported architecture: {0}")]
    UnsupportedArch(String),
    #[error("weight count mismatch: expected {expected}, got {got}")]
    WeightCountMismatch { expected: usize, got: usize },
    #[error("unknown activation: {0}")]
    UnknownActivation(String),
    #[error("config field missing: {0}")]
    MissingField(&'static str),
    #[error("unsupported feature: {0}")]
    Unsupported(&'static str),
}

// ---------------------------------------------------------------------------
// JSON loader
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct NamFile {
    pub version: Option<String>,
    pub architecture: String,
    pub config: serde_json::Value,
    pub metadata: Option<serde_json::Value>,
    pub weights: Vec<f32>,
}

pub fn load_from_json(json: &str) -> Result<NamFile, NamError> {
    let file: NamFile = serde_json::from_str(json)?;
    Ok(file)
}

// ---------------------------------------------------------------------------
// Building blocks: Conv1D + Conv1x1 (1×1 conv) with ring-buffer state for
// per-sample causal inference.
//
// Weight layout matches Conv1D::set_weights_ in NAM C++ exactly (out-major,
// then in, then kernel). Bias is appended after the dense matrix.
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Conv1D {
    pub in_ch: usize,
    pub out_ch: usize,
    pub kernel: usize,
    pub dilation: usize,
    pub has_bias: bool,
    /// `[kernel][out_ch][in_ch]` flattened. Reads happen as
    /// `weight[k * out_ch * in_ch + o * in_ch + i]`.
    pub weight: Vec<f32>,
    /// `[out_ch]` when `has_bias`, otherwise empty.
    pub bias: Vec<f32>,
    /// Ring buffer of past inputs, `[hist_frames][in_ch]` flattened.
    /// `hist_frames = (kernel - 1) * dilation + 1` — enough to cover the
    /// receptive field of one output sample.
    pub hist: Vec<f32>,
    pub hist_frames: usize,
    pub hist_pos: usize,
}

impl Conv1D {
    pub fn new(
        in_ch: usize,
        out_ch: usize,
        kernel: usize,
        dilation: usize,
        has_bias: bool,
    ) -> Self {
        let hist_frames = (kernel - 1) * dilation + 1;
        Self {
            in_ch,
            out_ch,
            kernel,
            dilation,
            has_bias,
            weight: vec![0.0; kernel * out_ch * in_ch],
            bias: if has_bias { vec![0.0; out_ch] } else { vec![] },
            hist: vec![0.0; hist_frames * in_ch],
            hist_frames,
            hist_pos: 0,
        }
    }

    /// Pull `weight_count + bias_count` floats from `it` in NAM order.
    /// Returns `Err` if the iterator runs out.
    pub fn load_weights(&mut self, it: &mut WeightCursor<'_>) -> Result<(), NamError> {
        // NAM order: for o, for i, for k → weight[k][o][i]
        for o in 0..self.out_ch {
            for i in 0..self.in_ch {
                for k in 0..self.kernel {
                    let idx = k * self.out_ch * self.in_ch + o * self.in_ch + i;
                    self.weight[idx] = it.next()?;
                }
            }
        }
        if self.has_bias {
            for b in 0..self.out_ch {
                self.bias[b] = it.next()?;
            }
        }
        Ok(())
    }

    /// Push one input frame into the history and compute the dot product.
    /// Writes `out_ch` floats into `out`.
    ///
    /// Hot path — this runs `out_ch × kernel × in_ch` MACs per sample
    /// per layer, so a Standard NAM does ~10 k MACs/sample. The
    /// optimisations below cut a ~85% CPU cost per channel down to
    /// something usable in REAPER:
    ///
    /// 1. **Frame offsets precomputed once per sample** (not per output
    ///    channel) — was `out_ch × kernel` modulo ops, now `kernel`.
    /// 2. **Outer loop over kernel taps, inner over (out_ch, in_ch)** —
    ///    cache-friendly contiguous read of `weight[k][*][*]` and
    ///    `hist[frame][*]`.
    /// 3. **Unchecked indexing** in the inner accumulator via
    ///    `slice::get_unchecked` — eliminates bounds checks the
    ///    compiler can't prove away (Vec indexing always re-checks).
    ///    SAFETY: every index below comes from a bounded loop counter
    ///    inside the buffer dimensions allocated at `new`.
    pub fn process(&mut self, input: &[f32], out: &mut [f32]) {
        debug_assert_eq!(input.len(), self.in_ch);
        debug_assert_eq!(out.len(), self.out_ch);
        let in_ch = self.in_ch;
        let out_ch = self.out_ch;
        let kernel = self.kernel;
        let dilation = self.dilation;
        let hf = self.hist_frames;
        let has_bias = self.has_bias;

        // 1) Write input into hist[hist_pos].
        let base = self.hist_pos * in_ch;
        self.hist[base..base + in_ch].copy_from_slice(input);

        // 2) Precompute per-kernel frame base offsets — one branchless
        //    wrap each instead of one modulo per (out_ch, kernel).
        //    Stack-allocate up to 16 kernel taps (real NAM uses 3..4);
        //    falls back to heap for the rare large-kernel models.
        let mut frame_bases_stack = [0usize; 16];
        let mut frame_bases_heap: Vec<usize>;
        let frame_bases: &mut [usize] = if kernel <= 16 {
            &mut frame_bases_stack[..kernel]
        } else {
            frame_bases_heap = vec![0; kernel];
            &mut frame_bases_heap[..]
        };
        for k in 0..kernel {
            let back = (kernel - 1 - k) * dilation;
            // Branchless wrap into [0, hf). Equivalent to (hist_pos +
            // hf - back) % hf when (back <= hist_pos + hf), which is
            // always true by construction (back ≤ (kernel-1)*dilation
            // ≤ hf - 1 ≤ hist_pos + hf).
            let mut frame = self.hist_pos + hf - back;
            while frame >= hf {
                frame -= hf;
            }
            frame_bases[k] = frame * in_ch;
        }

        // 3) Initialise output with bias (or zero).
        if has_bias {
            out.copy_from_slice(&self.bias);
        } else {
            for o in out.iter_mut() {
                *o = 0.0;
            }
        }

        // 4) Accumulate. Outer = kernel (small), inner = (out_ch, in_ch).
        //    Weight layout: `w[k][o][i]` flattened. For each tap we read
        //    a contiguous `out_ch * in_ch` block of weights and a
        //    contiguous `in_ch` slice of history — both cache-friendly.
        let weight = &self.weight[..];
        let hist = &self.hist[..];
        for k in 0..kernel {
            let wbase_k = k * out_ch * in_ch;
            let fbase = frame_bases[k];
            // SAFETY: fbase + in_ch ≤ hist.len() because frame < hf and
            // hist.len() == hf * in_ch. wbase_k + out_ch*in_ch ≤
            // weight.len() == kernel*out_ch*in_ch.
            unsafe {
                let w_tap = weight.as_ptr().add(wbase_k);
                let h_frame = hist.as_ptr().add(fbase);
                for o in 0..out_ch {
                    let mut acc = *out.get_unchecked(o);
                    let w_o = w_tap.add(o * in_ch);
                    // Inner dot product, in_ch ≤ 16 typical for NAM —
                    // rustc auto-vectorises this with SSE/NEON.
                    for i in 0..in_ch {
                        acc += *w_o.add(i) * *h_frame.add(i);
                    }
                    *out.get_unchecked_mut(o) = acc;
                }
            }
        }

        // 5) Advance ring head AFTER computing so the just-written frame
        //    is the "current" sample (k = kernel-1, back = 0).
        self.hist_pos += 1;
        if self.hist_pos >= hf {
            self.hist_pos = 0;
        }
    }

    pub fn reset(&mut self) {
        self.hist.fill(0.0);
        self.hist_pos = 0;
    }

    pub fn param_count(&self) -> usize {
        self.kernel * self.out_ch * self.in_ch + if self.has_bias { self.out_ch } else { 0 }
    }
}

/// 1×1 conv (no history needed — it's a per-sample matrix multiply).
#[derive(Clone)]
pub struct Conv1x1 {
    pub in_ch: usize,
    pub out_ch: usize,
    pub has_bias: bool,
    pub weight: Vec<f32>, // [out_ch][in_ch]
    pub bias: Vec<f32>,   // [out_ch] when has_bias
}

impl Conv1x1 {
    pub fn new(in_ch: usize, out_ch: usize, has_bias: bool) -> Self {
        Self {
            in_ch,
            out_ch,
            has_bias,
            weight: vec![0.0; out_ch * in_ch],
            bias: if has_bias { vec![0.0; out_ch] } else { vec![] },
        }
    }
    pub fn load_weights(&mut self, it: &mut WeightCursor<'_>) -> Result<(), NamError> {
        // Conv1D with kernel=1 — same loop, but the inner k loop is
        // length 1 so we can inline it.
        for o in 0..self.out_ch {
            for i in 0..self.in_ch {
                self.weight[o * self.in_ch + i] = it.next()?;
            }
        }
        if self.has_bias {
            for b in 0..self.out_ch {
                self.bias[b] = it.next()?;
            }
        }
        Ok(())
    }
    pub fn process(&self, input: &[f32], out: &mut [f32]) {
        debug_assert_eq!(input.len(), self.in_ch);
        debug_assert_eq!(out.len(), self.out_ch);
        let in_ch = self.in_ch;
        let out_ch = self.out_ch;
        // SAFETY: weight.len() == out_ch * in_ch, bias.len() == out_ch
        // (or 0), input.len() and out.len() asserted above.
        unsafe {
            let w = self.weight.as_ptr();
            let inp = input.as_ptr();
            let out_p = out.as_mut_ptr();
            let bias_p = if self.has_bias { self.bias.as_ptr() } else { std::ptr::null() };
            for o in 0..out_ch {
                let mut acc = if bias_p.is_null() { 0.0 } else { *bias_p.add(o) };
                let w_o = w.add(o * in_ch);
                for i in 0..in_ch {
                    acc += *w_o.add(i) * *inp.add(i);
                }
                *out_p.add(o) = acc;
            }
        }
    }
    pub fn param_count(&self) -> usize {
        self.out_ch * self.in_ch + if self.has_bias { self.out_ch } else { 0 }
    }
}

/// Cursor over a flat weight vector — gives a typed error when we run
/// off the end instead of panicking on out-of-bounds.
pub struct WeightCursor<'a> {
    data: &'a [f32],
    pos: usize,
}
impl<'a> WeightCursor<'a> {
    pub fn new(data: &'a [f32]) -> Self {
        Self { data, pos: 0 }
    }
    pub fn next(&mut self) -> Result<f32, NamError> {
        if self.pos >= self.data.len() {
            return Err(NamError::WeightCountMismatch {
                expected: self.pos + 1,
                got: self.data.len(),
            });
        }
        let v = self.data[self.pos];
        self.pos += 1;
        Ok(v)
    }
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
}

// ---------------------------------------------------------------------------
// WaveNet Layer
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Layer {
    pub channels: usize,
    pub bottleneck: usize,
    pub condition_size: usize,
    pub kernel: usize,
    pub dilation: usize,
    pub gating_mode: GatingMode,
    pub activation: Activation,
    pub secondary_activation: Activation,
    pub conv: Conv1D,
    pub input_mixin: Conv1x1,
    pub layer1x1: Option<Conv1x1>,
    /// Optional 1×1 conv that runs on the post-activation tensor before
    /// it's fed into the layer-array head accumulator. When active, the
    /// head input size equals `head1x1.out_channels` instead of
    /// `bottleneck`. NAM's `head1x1.active` flag.
    pub head1x1: Option<Conv1x1>,
    /// Number of values the layer pushes into the head accumulator —
    /// either `bottleneck` (no head1x1) or `head1x1.out_channels`.
    pub head_input_size: usize,
    /// Scratch: `[out_ch]` = `[2*bottleneck]` if gated/blended else `[bottleneck]`.
    scratch_conv: Vec<f32>,
    scratch_mixin: Vec<f32>,
    scratch_act: Vec<f32>,
    /// `[bottleneck]` — gate / blend activation output (when gating_mode != None).
    scratch_gate: Vec<f32>,
    scratch_l1x1: Vec<f32>,
    /// `[head_input_size]` — head input after optional head1x1.
    scratch_head: Vec<f32>,
}

impl Layer {
    pub fn new(
        channels: usize,
        bottleneck: usize,
        condition_size: usize,
        kernel: usize,
        dilation: usize,
        gating_mode: GatingMode,
        activation: Activation,
        secondary_activation: Activation,
        layer1x1_active: bool,
        head1x1_out_channels: Option<usize>,
    ) -> Self {
        let out_ch = if gating_mode == GatingMode::None {
            bottleneck
        } else {
            2 * bottleneck
        };
        let head1x1 = head1x1_out_channels.map(|out| Conv1x1::new(bottleneck, out, true));
        let head_input_size = head1x1
            .as_ref()
            .map(|c| c.out_ch)
            .unwrap_or(bottleneck);
        Self {
            channels,
            bottleneck,
            condition_size,
            kernel,
            dilation,
            gating_mode,
            activation,
            secondary_activation,
            conv: Conv1D::new(channels, out_ch, kernel, dilation, true),
            input_mixin: Conv1x1::new(condition_size, out_ch, false),
            layer1x1: if layer1x1_active {
                Some(Conv1x1::new(bottleneck, channels, true))
            } else {
                None
            },
            head1x1,
            head_input_size,
            scratch_conv: vec![0.0; out_ch],
            scratch_mixin: vec![0.0; out_ch],
            scratch_act: vec![0.0; bottleneck],
            scratch_gate: vec![0.0; bottleneck],
            scratch_l1x1: vec![0.0; channels],
            scratch_head: vec![0.0; head_input_size],
        }
    }

    pub fn load_weights(&mut self, it: &mut WeightCursor<'_>) -> Result<(), NamError> {
        // Order in NAM (model.cpp:135): conv, input_mixin, [layer1x1], [head1x1].
        self.conv.load_weights(it)?;
        self.input_mixin.load_weights(it)?;
        if let Some(ref mut l) = self.layer1x1 {
            l.load_weights(it)?;
        }
        if let Some(ref mut h) = self.head1x1 {
            h.load_weights(it)?;
        }
        Ok(())
    }

    /// Process one sample.
    /// - `input`:           `[channels]` — output of the previous layer (or stack input)
    /// - `condition`:       `[condition_size]` — usually the raw audio sample wrapped in a 1-vec
    /// - `residual_out`:    `[channels]` — input + layer1x1(activation) (or just input)
    /// - `head_input_out`:  `[head_input_size]` — head_input (post head1x1 if active)
    pub fn process(
        &mut self,
        input: &[f32],
        condition: &[f32],
        residual_out: &mut [f32],
        head_input_out: &mut [f32],
    ) {
        debug_assert_eq!(head_input_out.len(), self.head_input_size);
        // 1) Conv + input_mixin → scratch
        self.conv.process(input, &mut self.scratch_conv);
        self.input_mixin.process(condition, &mut self.scratch_mixin);
        for i in 0..self.scratch_conv.len() {
            self.scratch_conv[i] += self.scratch_mixin[i];
        }
        // 2) Activation according to gating_mode.
        match self.gating_mode {
            GatingMode::None => {
                for i in 0..self.bottleneck {
                    self.scratch_act[i] = self.activation.apply(self.scratch_conv[i]);
                }
            }
            GatingMode::Gated => {
                // y = activation(A) * secondary(B)
                for i in 0..self.bottleneck {
                    let a = self.activation.apply(self.scratch_conv[i]);
                    let b = self
                        .secondary_activation
                        .apply(self.scratch_conv[self.bottleneck + i]);
                    self.scratch_act[i] = a * b;
                }
            }
            GatingMode::Blended => {
                // alpha = secondary(B);  y = alpha * activation(A) + (1 - alpha) * A
                for i in 0..self.bottleneck {
                    let a_raw = self.scratch_conv[i];
                    let a_act = self.activation.apply(a_raw);
                    let alpha = self
                        .secondary_activation
                        .apply(self.scratch_conv[self.bottleneck + i]);
                    self.scratch_act[i] = alpha * a_act + (1.0 - alpha) * a_raw;
                }
            }
        }
        // 3) Head input — pass scratch_act through head1x1 if active.
        if let Some(ref h1x1) = self.head1x1 {
            h1x1.process(&self.scratch_act, head_input_out);
        } else {
            head_input_out.copy_from_slice(&self.scratch_act);
        }
        // 4) Residual = input + layer1x1(activation), or input alone if
        //    no layer1x1.
        if let Some(ref l1x1) = self.layer1x1 {
            l1x1.process(&self.scratch_act, &mut self.scratch_l1x1);
            for c in 0..self.channels {
                residual_out[c] = input[c] + self.scratch_l1x1[c];
            }
        } else {
            // bottleneck == channels required (config validation).
            for c in 0..self.channels {
                residual_out[c] = input[c] + self.scratch_act[c];
            }
        }
        let _ = self.scratch_gate;
        let _ = self.scratch_head;
    }

    pub fn reset(&mut self) {
        self.conv.reset();
    }
}

// ---------------------------------------------------------------------------
// LayerArray (stack)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct LayerArray {
    pub input_size: usize,
    pub condition_size: usize,
    pub channels: usize,
    pub bottleneck: usize,
    pub head_size: usize,
    pub head_kernel_size: usize,
    pub head_bias: bool,
    pub rechannel: Conv1x1,
    pub layers: Vec<Layer>,
    pub head_rechannel: Conv1D,
    /// Per-sample scratch buffers (no allocations during inference).
    scratch_layer_input: Vec<f32>,
    scratch_layer_residual: Vec<f32>,
    scratch_head_input: Vec<f32>,
    /// Accumulator for the head inputs across all layers in this array.
    head_accum: Vec<f32>,
    /// Output buffer for `head_rechannel.process` — `[head_size]`.
    head_out_buf: Vec<f32>,
}

impl LayerArray {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        input_size: usize,
        condition_size: usize,
        channels: usize,
        bottleneck: usize,
        head_size: usize,
        head_kernel_size: usize,
        head_bias: bool,
        kernel: usize,
        dilations: &[usize],
        gating_mode: GatingMode,
        activation: Activation,
        secondary_activation: Activation,
        layer1x1_active: bool,
        head1x1_out_channels: Option<usize>,
    ) -> Self {
        let layers: Vec<Layer> = dilations
            .iter()
            .map(|&d| {
                Layer::new(
                    channels,
                    bottleneck,
                    condition_size,
                    kernel,
                    d,
                    gating_mode,
                    activation,
                    secondary_activation,
                    layer1x1_active,
                    head1x1_out_channels,
                )
            })
            .collect();
        // Head input size matches the per-layer head_input_size (either
        // `bottleneck` or `head1x1.out_channels`). All layers in the
        // array share the same value.
        let head_input_size = layers
            .first()
            .map(|l| l.head_input_size)
            .unwrap_or(bottleneck);
        Self {
            input_size,
            condition_size,
            channels,
            bottleneck,
            head_size,
            head_kernel_size,
            head_bias,
            rechannel: Conv1x1::new(input_size, channels, false),
            layers,
            head_rechannel: Conv1D::new(head_input_size, head_size, head_kernel_size, 1, head_bias),
            scratch_layer_input: vec![0.0; channels],
            scratch_layer_residual: vec![0.0; channels],
            scratch_head_input: vec![0.0; head_input_size],
            head_accum: vec![0.0; head_input_size],
            head_out_buf: vec![0.0; head_size],
        }
    }

    pub fn load_weights(&mut self, it: &mut WeightCursor<'_>) -> Result<(), NamError> {
        // Order from NAM: _rechannel, _layers, _head_rechannel.
        self.rechannel.load_weights(it)?;
        for layer in self.layers.iter_mut() {
            layer.load_weights(it)?;
        }
        self.head_rechannel.load_weights(it)?;
        Ok(())
    }

    /// One-sample forward pass.
    /// - `stack_input`: `[input_size]` — output of the previous stack (or audio in for stack 0)
    /// - `condition`:   `[condition_size]` — conditioning (typically the raw audio input)
    /// - `prev_head`:   `Option<&[f32]>` — head output from previous stack (size = head_size)
    /// - `layer_out`:   `[channels]` — output residual of the last layer in this stack
    /// - `head_out`:    `[head_size]` — head_rechannel applied to the bottleneck-wide accumulator
    pub fn process(
        &mut self,
        stack_input: &[f32],
        condition: &[f32],
        prev_head: Option<&[f32]>,
        layer_out: &mut [f32],
        head_out: &mut [f32],
    ) {
        // 1) Rechannel input_size → channels.
        self.rechannel.process(stack_input, &mut self.scratch_layer_input);
        // 2) Zero accumulator, then sum head_input from each layer.
        self.head_accum.fill(0.0);
        for layer in self.layers.iter_mut() {
            layer.process(
                &self.scratch_layer_input,
                condition,
                &mut self.scratch_layer_residual,
                &mut self.scratch_head_input,
            );
            for i in 0..self.bottleneck {
                self.head_accum[i] += self.scratch_head_input[i];
            }
            // Next layer consumes this layer's residual.
            std::mem::swap(&mut self.scratch_layer_input, &mut self.scratch_layer_residual);
        }
        // 3) Last residual becomes the stack's layer output.
        layer_out.copy_from_slice(&self.scratch_layer_input);
        // 4) head_rechannel applied to the accumulated head input.
        // head_rechannel input is the bottleneck-wide (or head1x1.out_channels)
        // accumulator built up across all layers.
        self.head_rechannel.process(&self.head_accum, &mut self.head_out_buf);
        // 5) If we have a previous stack's head, add it on top.
        if let Some(prev) = prev_head {
            for i in 0..self.head_size {
                head_out[i] = self.head_out_buf[i] + prev[i];
            }
        } else {
            head_out.copy_from_slice(&self.head_out_buf);
        }
    }

    pub fn reset(&mut self) {
        for l in self.layers.iter_mut() {
            l.reset();
        }
        self.head_rechannel.reset();
    }

    pub fn param_count(&self) -> usize {
        let mut n = self.rechannel.param_count();
        for layer in &self.layers {
            n += layer.conv.param_count();
            n += layer.input_mixin.param_count();
            if let Some(l) = &layer.layer1x1 {
                n += l.param_count();
            }
            if let Some(h) = &layer.head1x1 {
                n += h.param_count();
            }
        }
        n += self.head_rechannel.param_count();
        n
    }
}

// ---------------------------------------------------------------------------
// WaveNet — top-level
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct WaveNet {
    pub in_channels: usize,
    pub head_scale: f32,
    pub stacks: Vec<LayerArray>,
    /// Scratch buffers — sized once so process() doesn't allocate.
    scratch_stack_in: Vec<f32>,
    scratch_stack_out: Vec<f32>,
    scratch_head_a: Vec<f32>,
    scratch_head_b: Vec<f32>,
    /// Single-sample condition vector reused per `process` call.
    cond_vec: Vec<f32>,
}

#[derive(Debug, Clone)]
pub struct LayerArrayParams {
    pub input_size: usize,
    pub condition_size: usize,
    pub channels: usize,
    pub bottleneck: usize,
    pub head_size: usize,
    pub head_kernel_size: usize,
    pub head_bias: bool,
    pub kernel: usize,
    pub dilations: Vec<usize>,
    pub gating_mode: GatingMode,
    pub activation: Activation,
    pub secondary_activation: Activation,
    pub layer1x1_active: bool,
    pub head1x1_out_channels: Option<usize>,
}

impl WaveNet {
    /// Build a WaveNet from per-stack params. Weights remain zero — call
    /// `load_weights` or `hand_tune_tube_preamp` next.
    pub fn from_params(in_channels: usize, stacks: &[LayerArrayParams], head_scale: f32) -> Self {
        let max_channels = stacks.iter().map(|s| s.channels).max().unwrap_or(1);
        let max_head_size = stacks.iter().map(|s| s.head_size).max().unwrap_or(1);
        let max_cond = stacks
            .iter()
            .map(|s| s.condition_size)
            .max()
            .unwrap_or(in_channels);
        Self {
            in_channels,
            head_scale,
            stacks: stacks
                .iter()
                .map(|p| {
                    LayerArray::new(
                        p.input_size,
                        p.condition_size,
                        p.channels,
                        p.bottleneck,
                        p.head_size,
                        p.head_kernel_size,
                        p.head_bias,
                        p.kernel,
                        &p.dilations,
                        p.gating_mode,
                        p.activation,
                        p.secondary_activation,
                        p.layer1x1_active,
                        p.head1x1_out_channels,
                    )
                })
                .collect(),
            scratch_stack_in: vec![0.0; max_channels.max(in_channels)],
            scratch_stack_out: vec![0.0; max_channels],
            scratch_head_a: vec![0.0; max_head_size],
            scratch_head_b: vec![0.0; max_head_size],
            cond_vec: vec![0.0; max_cond],
        }
    }

    /// Parse a `NamFile` into params + load all weights.
    pub fn from_nam_file(file: &NamFile) -> Result<Self, NamError> {
        if file.architecture != "WaveNet" {
            return Err(NamError::UnsupportedArch(file.architecture.clone()));
        }
        let cfg = &file.config;
        let layers = cfg
            .get("layers")
            .and_then(|v| v.as_array())
            .ok_or(NamError::MissingField("config.layers"))?;
        let head_scale = cfg
            .get("head_scale")
            .and_then(|v| v.as_f64())
            .ok_or(NamError::MissingField("config.head_scale"))? as f32;
        if cfg.get("head").is_some() && !cfg["head"].is_null() {
            return Err(NamError::Unsupported("post-stack head"));
        }
        let in_channels = cfg.get("in_channels").and_then(|v| v.as_u64()).unwrap_or(1) as usize;

        let mut stacks: Vec<LayerArrayParams> = Vec::with_capacity(layers.len());
        for lc in layers {
            let channels = field_usize(lc, "channels")?;
            let bottleneck = lc
                .get("bottleneck")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize)
                .unwrap_or(channels);
            let input_size = field_usize(lc, "input_size")?;
            let condition_size = field_usize(lc, "condition_size")?;
            // Head fields — prefer nested `head`, fall back to legacy
            // top-level `head_size` + `head_bias`.
            let (head_size, head_kernel_size, head_bias) = if let Some(h) = lc.get("head") {
                (
                    h.get("out_channels")
                        .and_then(|v| v.as_u64())
                        .ok_or(NamError::MissingField("layer.head.out_channels"))?
                        as usize,
                    h.get("kernel_size")
                        .and_then(|v| v.as_u64())
                        .ok_or(NamError::MissingField("layer.head.kernel_size"))?
                        as usize,
                    h.get("bias")
                        .and_then(|v| v.as_bool())
                        .ok_or(NamError::MissingField("layer.head.bias"))?,
                )
            } else {
                (
                    field_usize(lc, "head_size")?,
                    1usize,
                    lc.get("head_bias")
                        .and_then(|v| v.as_bool())
                        .ok_or(NamError::MissingField("layer.head_bias"))?,
                )
            };
            let kernel = field_usize(lc, "kernel_size")?;
            let dilations: Vec<usize> = lc
                .get("dilations")
                .and_then(|v| v.as_array())
                .ok_or(NamError::MissingField("layer.dilations"))?
                .iter()
                .filter_map(|v| v.as_u64().map(|x| x as usize))
                .collect();
            // `activation` may be a plain string ("Tanh") in legacy
            // models or a `{"type": "Tanh"}` object in newer ones.
            let activation_str = lc
                .get("activation")
                .and_then(|v| {
                    v.as_str()
                        .map(|s| s.to_string())
                        .or_else(|| v.get("type").and_then(|t| t.as_str()).map(|s| s.to_string()))
                })
                .ok_or(NamError::MissingField("layer.activation"))?;
            let activation = Activation::parse(&activation_str)?;
            // gating_mode — new format wins over legacy `gated: bool`.
            let (gating_mode, secondary_activation) = if let Some(gm) =
                lc.get("gating_mode").and_then(|v| v.as_str())
            {
                let mode = GatingMode::parse(gm)
                    .ok_or(NamError::UnknownActivation(gm.into()))?;
                // secondary_activation is required when gating_mode != none.
                let sec = if mode == GatingMode::None {
                    Activation::Sigmoid
                } else {
                    let sec_str = lc
                        .get("secondary_activation")
                        .and_then(|v| {
                            v.as_str().map(|s| s.to_string()).or_else(|| {
                                v.get("type")
                                    .and_then(|t| t.as_str())
                                    .map(|s| s.to_string())
                            })
                        })
                        .unwrap_or_else(|| "Sigmoid".into());
                    if sec_str.is_empty() {
                        Activation::Sigmoid
                    } else {
                        Activation::parse(&sec_str)?
                    }
                };
                (mode, sec)
            } else if lc.get("gated").and_then(|v| v.as_bool()).unwrap_or(false) {
                // Legacy `gated: true` maps to GATED + Sigmoid secondary.
                (GatingMode::Gated, Activation::Sigmoid)
            } else {
                (GatingMode::None, Activation::Sigmoid)
            };
            // layer1x1 defaults to active per NAM model.cpp:855.
            let layer1x1_active = lc
                .get("layer1x1")
                .and_then(|v| v.get("active"))
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            // head1x1: optional 1×1 conv on the per-layer head output.
            let head1x1_out_channels = lc
                .get("head1x1")
                .filter(|v| {
                    v.get("active")
                        .and_then(|a| a.as_bool())
                        .unwrap_or(false)
                })
                .and_then(|v| v.get("out_channels"))
                .and_then(|v| v.as_u64())
                .map(|x| x as usize);
            // Reject FiLM — still out of scope (no community models use it).
            for unsupported in &[
                "conv_pre_film",
                "conv_post_film",
                "input_mixin_pre_film",
                "input_mixin_post_film",
                "activation_pre_film",
                "activation_post_film",
                "layer1x1_post_film",
                "head1x1_post_film",
            ] {
                if let Some(v) = lc.get(*unsupported) {
                    if !v.is_null() && v != &serde_json::Value::Bool(false) {
                        return Err(NamError::Unsupported("FiLM"));
                    }
                }
            }
            // Reject grouped / depthwise convs — not implemented yet.
            for unsupported in &["groups_input", "groups_input_mixin"] {
                if let Some(v) = lc.get(*unsupported) {
                    if v.as_u64().map(|x| x as usize).unwrap_or(1) != 1 {
                        return Err(NamError::Unsupported("grouped conv"));
                    }
                }
            }
            stacks.push(LayerArrayParams {
                input_size,
                condition_size,
                channels,
                bottleneck,
                head_size,
                head_kernel_size,
                head_bias,
                kernel,
                dilations,
                gating_mode,
                activation,
                secondary_activation,
                layer1x1_active,
                head1x1_out_channels,
            });
        }

        let mut net = WaveNet::from_params(in_channels, &stacks, head_scale);
        let mut cursor = WeightCursor::new(&file.weights);
        for stack in net.stacks.iter_mut() {
            stack.load_weights(&mut cursor)?;
        }
        // Consume the trailing `head_scale` weight (NAM appends it after
        // all stacks; we already parsed it from config but the C++
        // reference reads it from the weights iterator and overrides).
        let trailing = cursor.next()?;
        net.head_scale = trailing;
        if cursor.remaining() != 0 {
            return Err(NamError::WeightCountMismatch {
                expected: file.weights.len() - cursor.remaining(),
                got: file.weights.len(),
            });
        }
        Ok(net)
    }

    /// One-sample forward pass.
    pub fn process(&mut self, x: f32) -> f32 {
        // Condition vector is just the audio sample (in_channels = 1 for
        // standard NAM). Extend with zeros if condition_size > 1.
        self.cond_vec.fill(0.0);
        self.cond_vec[0] = x;

        // Stack 0 input is the raw audio.
        self.scratch_stack_in[0] = x;
        for c in 1..self.scratch_stack_in.len() {
            self.scratch_stack_in[c] = 0.0;
        }

        // Locally borrow the two head buffers — `prev` carries the
        // previous stack's head into the next stack, `cur` receives this
        // stack's output. After each iteration we swap them.
        let prev_head_buf = &mut self.scratch_head_a;
        let cur_head_buf = &mut self.scratch_head_b;
        prev_head_buf.fill(0.0);
        cur_head_buf.fill(0.0);

        let mut have_prev = false;
        let mut prev_head_size = 0usize;
        let mut last_head_size = 0usize;

        let stacks = &mut self.stacks;
        let stack_in = &mut self.scratch_stack_in;
        let stack_out = &mut self.scratch_stack_out;
        let cond = &self.cond_vec;

        // Two non-aliasing buffers — we ping-pong between them by
        // swapping the pointers (cheap, no copy).
        let mut p_buf: &mut Vec<f32> = prev_head_buf;
        let mut c_buf: &mut Vec<f32> = cur_head_buf;

        for stack in stacks.iter_mut() {
            let input_slice = &stack_in[..stack.input_size];
            let cond_slice = &cond[..stack.condition_size];
            let prev = if have_prev {
                Some(&p_buf[..prev_head_size])
            } else {
                None
            };
            stack.process(
                input_slice,
                cond_slice,
                prev,
                &mut stack_out[..stack.channels],
                &mut c_buf[..stack.head_size],
            );
            // Copy new stack output into the stack-in slot for next iter.
            for c in 0..stack.channels {
                stack_in[c] = stack_out[c];
            }
            std::mem::swap(&mut p_buf, &mut c_buf);
            prev_head_size = stack.head_size;
            last_head_size = stack.head_size;
            have_prev = true;
        }
        let _ = last_head_size;
        // After the swap, `p_buf` holds the most recent stack's output.
        p_buf[0] * self.head_scale
    }

    pub fn reset(&mut self) {
        for s in self.stacks.iter_mut() {
            s.reset();
        }
    }

    pub fn param_count(&self) -> usize {
        self.stacks.iter().map(|s| s.param_count()).sum::<usize>() + 1
    }

    /// Hand-init weights to a "tube preamp" sound — used as the default
    /// when no `.nam` file is loaded.
    pub fn hand_tune_tube_preamp(&mut self) {
        for stack in self.stacks.iter_mut() {
            // _rechannel: identity-ish (just gain-of-1 from input to first channel).
            for o in 0..stack.channels {
                for i in 0..stack.input_size {
                    let v = if i == 0 {
                        1.0 / (stack.channels as f32).sqrt()
                    } else {
                        0.0
                    };
                    stack.rechannel.weight[o * stack.input_size + i] = v;
                }
            }
            for layer in stack.layers.iter_mut() {
                // _conv: weighted identity at the kernel centre.
                let centre = layer.kernel / 2;
                for o in 0..layer.conv.out_ch {
                    for i in 0..layer.conv.in_ch {
                        for k in 0..layer.conv.kernel {
                            let idx = k * layer.conv.out_ch * layer.conv.in_ch
                                + o * layer.conv.in_ch
                                + i;
                            layer.conv.weight[idx] =
                                if (o % layer.conv.in_ch == i) && k == centre {
                                    0.6
                                } else {
                                    0.0
                                };
                        }
                    }
                    if layer.conv.has_bias {
                        layer.conv.bias[o] = 0.0;
                    }
                }
                // _input_mixin: small DC bias so the activation gets pushed off centre.
                for o in 0..layer.input_mixin.out_ch {
                    for i in 0..layer.input_mixin.in_ch {
                        layer.input_mixin.weight[o * layer.input_mixin.in_ch + i] = 0.1;
                    }
                }
                // _layer1x1: identity-like — preserves residual path.
                if let Some(ref mut l1) = layer.layer1x1 {
                    for o in 0..l1.out_ch {
                        for i in 0..l1.in_ch {
                            l1.weight[o * l1.in_ch + i] = if o == i { 0.2 } else { 0.0 };
                        }
                        if l1.has_bias {
                            l1.bias[o] = 0.0;
                        }
                    }
                }
            }
            // _head_rechannel: average bottleneck → head_size.
            for o in 0..stack.head_rechannel.out_ch {
                for i in 0..stack.head_rechannel.in_ch {
                    let idx = o * stack.head_rechannel.in_ch + i;
                    stack.head_rechannel.weight[idx] = 1.0 / stack.head_rechannel.in_ch as f32;
                }
                if stack.head_rechannel.has_bias {
                    stack.head_rechannel.bias[o] = 0.0;
                }
            }
        }
        self.head_scale = 1.0;
    }
}

fn field_usize(v: &serde_json::Value, name: &'static str) -> Result<usize, NamError> {
    v.get(name)
        .and_then(|x| x.as_u64())
        .map(|x| x as usize)
        .ok_or(NamError::MissingField(name))
}

// ---------------------------------------------------------------------------
// LSTM — ported from NAM/lstm.cpp.
//
// Each cell:
//   xh = [input || hidden]        // length (input_size + hidden_size)
//   ifgo = W * xh + b             // length 4*hidden_size
//   c = sigmoid(f) * c + sigmoid(i) * tanh(g)
//   h = sigmoid(o) * tanh(c)
//
// Stack of cells: cell[0] receives raw input, cell[k>0] receives the
// previous cell's hidden state. After the last cell:
//   output = head_weight * hidden + head_bias
//
// Weight order per cell (matches NAM `LSTMCell` ctor at lstm.cpp:9):
//   W (row-major), b, h_init, c_init.
// Followed at the WaveNet level by head_weight (row-major) + head_bias.
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct LstmCell {
    pub input_size: usize,
    pub hidden_size: usize,
    /// Row-major `[4*hidden][input+hidden]`.
    pub w: Vec<f32>,
    /// `[4*hidden]`
    pub b: Vec<f32>,
    /// Concatenated `[input || hidden]` state — first `input_size` overwritten
    /// per sample; last `hidden_size` is the persistent hidden state.
    pub xh: Vec<f32>,
    /// Cell state, `[hidden]`.
    pub c: Vec<f32>,
    /// Scratch buffer for `ifgo = W @ xh + b`, `[4*hidden]`.
    ifgo: Vec<f32>,
}

impl LstmCell {
    pub fn new(input_size: usize, hidden_size: usize) -> Self {
        Self {
            input_size,
            hidden_size,
            w: vec![0.0; 4 * hidden_size * (input_size + hidden_size)],
            b: vec![0.0; 4 * hidden_size],
            xh: vec![0.0; input_size + hidden_size],
            c: vec![0.0; hidden_size],
            ifgo: vec![0.0; 4 * hidden_size],
        }
    }

    pub fn load_weights(&mut self, it: &mut WeightCursor<'_>) -> Result<(), NamError> {
        // W in row-major (PyTorch order). NAM C++ explicitly says
        // `Assign in row-major because that's how PyTorch goes.`
        let cols = self.input_size + self.hidden_size;
        for r in 0..(4 * self.hidden_size) {
            for c in 0..cols {
                self.w[r * cols + c] = it.next()?;
            }
        }
        for i in 0..self.b.len() {
            self.b[i] = it.next()?;
        }
        // Initial hidden state (h goes into `xh[input_size..]`).
        for i in 0..self.hidden_size {
            self.xh[self.input_size + i] = it.next()?;
        }
        // Initial cell state.
        for i in 0..self.hidden_size {
            self.c[i] = it.next()?;
        }
        Ok(())
    }

    /// Process one input sample. Returns a slice of the new hidden state.
    pub fn process(&mut self, input: &[f32]) -> &[f32] {
        debug_assert_eq!(input.len(), self.input_size);
        // Write input into the front of xh; hidden stays in the tail.
        self.xh[..self.input_size].copy_from_slice(input);
        // ifgo = W * xh + b
        let cols = self.input_size + self.hidden_size;
        for r in 0..self.ifgo.len() {
            let mut acc = self.b[r];
            for c in 0..cols {
                acc += self.w[r * cols + c] * self.xh[c];
            }
            self.ifgo[r] = acc;
        }
        // Gate offsets:  i = [0..h], f = [h..2h], g = [2h..3h], o = [3h..4h]
        let h = self.hidden_size;
        for i in 0..h {
            let i_gate = sigmoid(self.ifgo[i]);
            let f_gate = sigmoid(self.ifgo[h + i]);
            let g_gate = self.ifgo[2 * h + i].tanh();
            // c = f * c + i * g
            self.c[i] = f_gate * self.c[i] + i_gate * g_gate;
        }
        for i in 0..h {
            let o_gate = sigmoid(self.ifgo[3 * h + i]);
            // h = o * tanh(c). Stored into xh tail so the next call reads
            // it as part of [input || hidden].
            self.xh[self.input_size + i] = o_gate * self.c[i].tanh();
        }
        &self.xh[self.input_size..]
    }

    pub fn reset(&mut self) {
        for v in self.xh[self.input_size..].iter_mut() {
            *v = 0.0;
        }
        self.c.fill(0.0);
    }
}

#[inline]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

#[derive(Clone)]
pub struct Lstm {
    pub in_channels: usize,
    pub out_channels: usize,
    pub hidden_size: usize,
    pub layers: Vec<LstmCell>,
    /// `[out_channels][hidden_size]` row-major.
    pub head_weight: Vec<f32>,
    /// `[out_channels]`.
    pub head_bias: Vec<f32>,
    /// Scratch input vector reused per sample.
    in_vec: Vec<f32>,
}

impl Lstm {
    pub fn new(
        in_channels: usize,
        out_channels: usize,
        num_layers: usize,
        input_size: usize,
        hidden_size: usize,
    ) -> Self {
        let layers = (0..num_layers)
            .map(|i| LstmCell::new(if i == 0 { input_size } else { hidden_size }, hidden_size))
            .collect();
        Self {
            in_channels,
            out_channels,
            hidden_size,
            layers,
            head_weight: vec![0.0; out_channels * hidden_size],
            head_bias: vec![0.0; out_channels],
            in_vec: vec![0.0; input_size.max(in_channels)],
        }
    }

    pub fn from_nam_file(file: &NamFile) -> Result<Self, NamError> {
        if file.architecture != "LSTM" {
            return Err(NamError::UnsupportedArch(file.architecture.clone()));
        }
        let cfg = &file.config;
        let num_layers = field_usize(cfg, "num_layers")?;
        let input_size = field_usize(cfg, "input_size")?;
        let hidden_size = field_usize(cfg, "hidden_size")?;
        let in_channels = cfg.get("in_channels").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
        let out_channels = cfg
            .get("out_channels")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as usize;
        let mut me = Self::new(in_channels, out_channels, num_layers, input_size, hidden_size);
        let mut cursor = WeightCursor::new(&file.weights);
        for layer in me.layers.iter_mut() {
            layer.load_weights(&mut cursor)?;
        }
        // head_weight row-major (out × hidden), then head_bias.
        for r in 0..out_channels {
            for h in 0..hidden_size {
                me.head_weight[r * hidden_size + h] = cursor.next()?;
            }
        }
        for r in 0..out_channels {
            me.head_bias[r] = cursor.next()?;
        }
        if cursor.remaining() != 0 {
            return Err(NamError::WeightCountMismatch {
                expected: file.weights.len() - cursor.remaining(),
                got: file.weights.len(),
            });
        }
        Ok(me)
    }

    /// One-sample forward. Mono input → mono output (`out_channels=1`
    /// for typical NAM LSTM, but we still write all out channels into
    /// the same scalar return — the first channel is what NAM uses).
    pub fn process(&mut self, x: f32) -> f32 {
        if self.layers.is_empty() {
            return x;
        }
        self.in_vec[0] = x;
        let input_size = self.layers[0].input_size;
        // Layer 0 reads raw input.
        let first_input_len = input_size;
        let mut hidden_ptr_kind = 0; // 0 = use in_vec, 1 = use previous layer hidden
        let mut prev_hidden_len = 0;

        // We can't borrow two layers at once, so step through indices.
        // Each layer reads either the raw input or the previous layer's
        // hidden state (which is owned by `self.layers[prev]`).
        for idx in 0..self.layers.len() {
            if idx == 0 {
                let _ = self.layers[idx].process(&self.in_vec[..first_input_len]);
            } else {
                // Borrow split: prev hidden lives in layers[idx-1].xh tail.
                let (front, back) = self.layers.split_at_mut(idx);
                let prev = &front[idx - 1];
                let prev_hidden = &prev.xh[prev.input_size..];
                back[0].process(prev_hidden);
            }
            prev_hidden_len = self.layers[idx].hidden_size;
        }
        let _ = (hidden_ptr_kind, prev_hidden_len);

        // Output = head_weight * last_hidden + head_bias  →  scalar (use
        // out channel 0 since LSTM NAM models always produce mono).
        let last = self.layers.last().unwrap();
        let hidden = &last.xh[last.input_size..];
        let mut y = self.head_bias[0];
        for h in 0..self.hidden_size {
            y += self.head_weight[h] * hidden[h];
        }
        y
    }

    pub fn reset(&mut self) {
        for l in self.layers.iter_mut() {
            l.reset();
        }
    }

    pub fn param_count(&self) -> usize {
        let mut n = 0;
        for l in &self.layers {
            n += l.w.len() + l.b.len() + l.hidden_size + l.hidden_size; // W + b + h_init + c_init
        }
        n += self.head_weight.len() + self.head_bias.len();
        n
    }
}

// ---------------------------------------------------------------------------
// Linear — port of NAM/dsp.cpp::Linear. A `receptive_field`-tap FIR filter.
// Used in NAM for identity / Wiener-filter tests and the simplest possible
// "amp model" (literally a one-tap linear gain). Weight order matches NAM:
//   y[n] = bias + sum_{k=0..rf-1} weights[k] * x[n-k]
// (NAM internally stores them in reverse so a single dot product works;
//  we do the equivalent walk over the ring buffer.)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Linear {
    pub receptive_field: usize,
    pub has_bias: bool,
    /// `weights[k]` multiplies `x[n-k]`. Length = `receptive_field`.
    pub weights: Vec<f32>,
    pub bias: f32,
    /// Ring buffer of past inputs, length = `receptive_field`.
    /// `pos` points at the slot the next sample will overwrite.
    history: Vec<f32>,
    pos: usize,
}

impl Linear {
    pub fn from_nam_file(file: &NamFile) -> Result<Self, NamError> {
        if file.architecture != "Linear" {
            return Err(NamError::UnsupportedArch(file.architecture.clone()));
        }
        let receptive_field = field_usize(&file.config, "receptive_field")?;
        let has_bias = file
            .config
            .get("bias")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let expected = receptive_field + if has_bias { 1 } else { 0 };
        if file.weights.len() != expected {
            return Err(NamError::WeightCountMismatch {
                expected,
                got: file.weights.len(),
            });
        }
        let weights = file.weights[..receptive_field].to_vec();
        let bias = if has_bias { file.weights[receptive_field] } else { 0.0 };
        Ok(Self {
            receptive_field,
            has_bias,
            weights,
            bias,
            history: vec![0.0; receptive_field.max(1)],
            pos: 0,
        })
    }

    pub fn process(&mut self, x: f32) -> f32 {
        // Write input into the ring at pos (overwrites the oldest sample).
        self.history[self.pos] = x;
        // Sum: weights[k] * x[n-k]. The just-written value is x[n] at
        // `history[pos]`; one step back (k=1) is at `history[pos-1 mod rf]`.
        let rf = self.receptive_field;
        let mut acc = self.bias;
        for k in 0..rf {
            let idx = (self.pos + rf - k) % rf;
            acc += self.weights[k] * self.history[idx];
        }
        self.pos = (self.pos + 1) % rf.max(1);
        acc
    }

    pub fn reset(&mut self) {
        self.history.fill(0.0);
        self.pos = 0;
    }

    pub fn param_count(&self) -> usize {
        self.receptive_field + if self.has_bias { 1 } else { 0 }
    }
}

// ---------------------------------------------------------------------------
// `NamModel` — uniform handle that the plugin can hold and process
// regardless of whether the loaded `.nam` is WaveNet, LSTM, or Linear.
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub enum NamModel {
    WaveNet(WaveNet),
    Lstm(Lstm),
    Linear(Linear),
}

impl NamModel {
    pub fn from_nam_file(file: &NamFile) -> Result<Self, NamError> {
        match file.architecture.as_str() {
            "WaveNet" => Ok(NamModel::WaveNet(WaveNet::from_nam_file(file)?)),
            "LSTM" => Ok(NamModel::Lstm(Lstm::from_nam_file(file)?)),
            "Linear" => Ok(NamModel::Linear(Linear::from_nam_file(file)?)),
            other => Err(NamError::UnsupportedArch(other.into())),
        }
    }
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        match self {
            NamModel::WaveNet(w) => w.process(x),
            NamModel::Lstm(l) => l.process(x),
            NamModel::Linear(l) => l.process(x),
        }
    }
    pub fn reset(&mut self) {
        match self {
            NamModel::WaveNet(w) => w.reset(),
            NamModel::Lstm(l) => l.reset(),
            NamModel::Linear(l) => l.reset(),
        }
    }
    pub fn param_count(&self) -> usize {
        match self {
            NamModel::WaveNet(w) => w.param_count(),
            NamModel::Lstm(l) => l.param_count(),
            NamModel::Linear(l) => l.param_count(),
        }
    }
    pub fn arch_name(&self) -> &'static str {
        match self {
            NamModel::WaveNet(_) => "WaveNet",
            NamModel::Lstm(_) => "LSTM",
            NamModel::Linear(_) => "Linear",
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn standard_default_net() -> WaveNet {
        // Tiny smoke-test net mirroring the NAM Standard topology.
        let stacks = vec![
            LayerArrayParams {
                input_size: 1,
                condition_size: 1,
                channels: 4,
                bottleneck: 4,
                head_size: 4,
                head_kernel_size: 1,
                head_bias: false,
                kernel: 3,
                dilations: vec![1, 2, 4, 8],
                gating_mode: GatingMode::None,
                activation: Activation::Tanh,
                secondary_activation: Activation::Sigmoid,
                layer1x1_active: true,
                head1x1_out_channels: None,
            },
            LayerArrayParams {
                input_size: 4,
                condition_size: 1,
                channels: 4,
                bottleneck: 4,
                head_size: 1,
                head_kernel_size: 1,
                head_bias: true,
                kernel: 3,
                dilations: vec![1, 2, 4, 8],
                gating_mode: GatingMode::None,
                activation: Activation::Tanh,
                secondary_activation: Activation::Sigmoid,
                layer1x1_active: true,
                head1x1_out_channels: None,
            },
        ];
        let mut net = WaveNet::from_params(1, &stacks, 0.02);
        net.hand_tune_tube_preamp();
        net
    }

    #[test]
    fn wavenet_produces_finite_output_on_sine() {
        let mut net = standard_default_net();
        for n in 0..512 {
            let x = (n as f32 * 0.1).sin() * 0.3;
            let y = net.process(x);
            assert!(y.is_finite() && y.abs() < 10.0, "y={y} at n={n}");
        }
    }

    #[test]
    fn weight_count_for_standard_matches_reference() {
        // wavenet_a1_standard.nam: 2 stacks, channels=[16,8], head_size=[8,1],
        // dilations 10 each, head_bias=[false, true], layer1x1 active.
        let stacks = vec![
            LayerArrayParams {
                input_size: 1, condition_size: 1, channels: 16, bottleneck: 16,
                head_size: 8, head_kernel_size: 1, head_bias: false,
                kernel: 3,
                dilations: vec![1,2,4,8,16,32,64,128,256,512],
                gating_mode: GatingMode::None, activation: Activation::Tanh,
                secondary_activation: Activation::Sigmoid,
                layer1x1_active: true, head1x1_out_channels: None,
            },
            LayerArrayParams {
                input_size: 16, condition_size: 1, channels: 8, bottleneck: 8,
                head_size: 1, head_kernel_size: 1, head_bias: true,
                kernel: 3,
                dilations: vec![1,2,4,8,16,32,64,128,256,512],
                gating_mode: GatingMode::None, activation: Activation::Tanh,
                secondary_activation: Activation::Sigmoid,
                layer1x1_active: true, head1x1_out_channels: None,
            },
        ];
        let net = WaveNet::from_params(1, &stacks, 0.02);
        // 1 trailing head_scale, + per-stack params.
        assert_eq!(net.param_count(), 13802);
    }

    #[test]
    fn loads_reference_wavenet_a1_standard_nam() {
        let p: PathBuf = std::env::var("HOME").map(PathBuf::from).unwrap()
            .join(".superduper-dsp/nam/wavenet_a1_standard.nam");
        if !p.exists() {
            eprintln!("skip: reference .nam not present at {:?}", p);
            return;
        }
        let s = std::fs::read_to_string(&p).expect("read");
        let f = load_from_json(&s).expect("parse");
        let mut net = WaveNet::from_nam_file(&f).expect("build wavenet");
        // Pre-warm + smoke audio: drive a 200 Hz sine through ~5000 samples.
        for n in 0..5000 {
            let x = (n as f32 / 48_000.0 * 2.0 * std::f32::consts::PI * 200.0).sin() * 0.3;
            let y = net.process(x);
            assert!(y.is_finite(), "non-finite at {n}: y={y}");
        }
    }

    #[test]
    fn wavenet_rejects_lstm() {
        let json = r#"{"architecture":"LSTM","config":{},"weights":[]}"#;
        let file = load_from_json(json).unwrap();
        match WaveNet::from_nam_file(&file) {
            Err(NamError::UnsupportedArch(_)) => {}
            other => panic!("expected UnsupportedArch, got {:?}", other.is_ok()),
        }
    }

    #[test]
    fn lstm_weight_count_for_example_matches_reference() {
        // example lstm.nam: 1 layer, input_size=1, hidden_size=3, out_channels=1
        // W: 4*3 × (1+3) = 48, b: 4*3 = 12, h: 3, c: 3, head_w: 3, head_b: 1
        // total = 48 + 12 + 3 + 3 + 3 + 1 = 70
        let lstm = Lstm::new(1, 1, 1, 1, 3);
        assert_eq!(lstm.param_count(), 70);
    }

    #[test]
    fn loads_reference_lstm_example_nam() {
        let p: PathBuf = std::env::var("HOME").map(PathBuf::from).unwrap()
            .join(".superduper-dsp/nam/lstm_example.nam");
        if !p.exists() {
            eprintln!("skip: reference .nam not present at {:?}", p);
            return;
        }
        let s = std::fs::read_to_string(&p).expect("read");
        let f = load_from_json(&s).expect("parse");
        let mut net = NamModel::from_nam_file(&f).expect("build");
        assert_eq!(net.arch_name(), "LSTM");
        for n in 0..5000 {
            let x = (n as f32 / 48_000.0 * 2.0 * std::f32::consts::PI * 200.0).sin() * 0.3;
            let y = net.process(x);
            assert!(y.is_finite(), "non-finite at {n}: y={y}");
        }
    }

    #[test]
    fn nam_model_dispatch_picks_arch() {
        let json = r#"{"architecture":"FooBar","config":{},"weights":[]}"#;
        let f = load_from_json(json).unwrap();
        match NamModel::from_nam_file(&f) {
            Err(NamError::UnsupportedArch(s)) => assert_eq!(s, "FooBar"),
            _ => panic!("expected UnsupportedArch"),
        }
    }
}
