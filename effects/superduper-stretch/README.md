# SuperDuper Stretch

Extreme time-stretch — PaulStretch, running live in your DAW. Four seconds of
singing becomes a minute of weather.

## The trick

Paul Nasca's insight was that to stretch by 8× you do **not** need to preserve
phase. Take a long window, keep its magnitude spectrum, **throw the phases away
and replace them with noise**, resynthesise, and overlap-add at a bigger hop than
you read with. Because the phases are random the frames cannot comb or cancel —
which is why an extreme ratio sounds glassy and smooth here, where a phase
vocoder would sound metallic and flangy.

`Tonal` blends the random phase back toward the analysed phase, so this one
plugin covers the whole range from a plain slow-down (Tonal 1) to the full
ambient wash (Tonal 0). Around 0.2 is the sweet spot for turning a sung note into
a pad that still has a legible pitch.

## Live vs Freeze

Stretching by N× consumes input N times slower than it emits output, so a
real-time stretcher has to decide what happens when the read head falls behind.
Both honest answers are here:

- **Live** — the read head trails the write head and, when it would fall off the
  end of the 12-second ring, skips forward to half a buffer behind. You hear a
  continuous smear of the recent past with an occasional jump. Good for a pad
  under a live source.
- **Freeze** — capture stops and the read head circles the last `Length` seconds
  forever. Sing one note, hit Freeze (or a sustain pedal, CC 64), and it becomes
  an infinite pad.

## Parameters

`Stretch` — the ratio, 1× to 50×.
`Window` — how much time each frame sees, 85 ms to 1.37 s. **This is the "how
smeared" control**: short keeps rhythm and identity, long is pure wash. It's
stepped rather than continuous so every value has a pre-built FFT plan and
changing it never allocates on the audio thread; the display shows ms, because
"16384" tells a musician nothing.
`Tonal` — 0 = random phase (classic smear), 1 = analysed phase (slow motion).
`Smooth` — frequency-proportional magnitude blur. Removes the identity of vowels
and instruments, leaves colour.
`Pitch` — spectral shift ±24 st, so ±12 gives an octave wash or a sub bed without
a separate pitch shifter.
`Freeze` / `Length` — stop capturing; how much of the take the loop wanders over.
`Mix` / `Output` / `Preset`.

## No latency compensation — by design

This plugin reports **zero** latency. A stretched signal isn't sample-aligned
with its input in any meaningful sense, so there is nothing for the DAW's PDC to
correct. Don't put it on a parallel bus expecting phase coherence — use it as a
send/return or on its own track.

## Chain tips

- **Stretch → Formant** is the "voice becomes kubyz" pair: this builds the
  endless bed out of your voice, Formant makes the bed pronounce vowels.
- Stretch → **Supermass** for infinite ambient.
- **Granular** after Stretch adds motion on top of the wash; before it, you're
  stretching a cloud.

## Tests

```bash
cargo test --release -p superduper-stretch
```

`dsp_smoke` measures the read head crawling at exactly 1/Stretch (0.1024 s per
second at 10×, target 0.1), that a 440 Hz tone stretched 8× still peaks at
436.5 Hz (phase randomisation must not move energy in frequency), that the
incoherent-OLA make-up keeps the output within 2 dB of the input, that Freeze
sustains a captured tone for seconds with no input, and that switching window
size mid-stream can't blow up the accumulator.
