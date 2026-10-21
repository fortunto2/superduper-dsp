<!-- Rules in our own words, after Mike Senior, *Mixing Secrets for the Small Studio*
     (Focal Press, 2011), cited as (MS chN). No book text here: the book and the full
     per-chapter notes stay local, see SKILL.md → Reference. -->
## Mixing method (after Mike Senior, *Mixing Secrets for the Small Studio*)

Balance is the goal and every process has to earn its place by making the balance steadier. A track
whose level sits still against the others needs nothing more than a high-pass filter and a fader.
A track that keeps wanting a different level tells you what to reach for. Level that changes over
time calls for dynamics; one frequency region that needs a different level from the rest calls for EQ.

### 0. Before touching a fader
- Work in a clean copy of the project. Render live MIDI instruments to audio but keep the MIDI source
  muted, so note statistics and transposition stay available (MS ch5).
- Short, unique track names, a fixed track order, folders for groups, named section markers, tempo map
  checked against the music (MS ch5).
- Split a part onto separate tracks when its job changes between sections, e.g. picked verse vs
  strummed chorus guitar (MS ch5, ch8).
- Arrangement beats processing. Keep roughly three things in focus at once. Mute or thin the rest.
  Where two parts fight in the same register, move one (transpose or re-voice the MIDI) or drop it
  rather than carving EQ (MS ch7). Check with `midi_note_stats` overlap plus `mix_masking`.

### 1. Order of work
1. Rank the song sections. Mix the biggest one (usually the last chorus) first, then make each earlier
   section a little smaller. If a later section sounds weak, take things out of the one before it
   instead of pushing the climax (MS ch8).
2. Rank tracks by how much they matter to the *sound* of that section (rock: drums and bass; pop: lead
   vocal; dance: kick, bass, hook). Bring them in one at a time in that order. Each newcomer only gets
   the room that is left (MS ch8, ch11).
3. For every track: routing → polarity/phase → high-pass → pan → level, then processing only if the
   level still won't settle (MS ch8, ch15).
4. After all tracks are in: blend/size effects, buss compression, section automation, then detailed
   rides, then referencing rounds (MS ch16, ch19).
5. Return to an earlier track whenever a later one makes it unsteady. Small moves, many of them. If a
   chain isn't working, strip it and start again (MS ch15).

### 2. Low end
- High-pass almost everything at a frequency found in context, with gentle slopes (6–18 dB/oct, no
  resonant peak); drums slightly lower than the point where their thump starts to change (MS ch8).
- Let kick and bass own the region below ~100 Hz. Remove it from guitars, pads, keys, often up to
  ~150 Hz (MS ch3). Verify: `mix_masking` should show no pairs except kick/bass in the lowest bands.
- Inaudible subsonic energy still eats headroom; look at the 20–40 Hz bands per track (MS ch3).
- For dependable weight, add a triggered kick sample or a mono MIDI sub (sine or triangle, an octave
  below, tightly controlled velocity) rather than a subharmonic plugin, and avoid stacking it on the
  original fundamental (MS ch12).

### 3. Panning and mono
- Kick, snare, bass and lead vocal in the centre. Balance a part panned to one side with a similar
  part on the other (MS ch8).
- Every stereo trick loses something in mono: the Sides signal disappears, and short Haas delays cancel
  harder the shorter they are. Never make a fake double by delaying a copy. Reorder repeated sections
  of a duplicate instead (MS ch2, ch18).
- Until a correlation meter exists, check mono by muting Sides on the master (mid/side plugin) or
  rendering mono, and compare per-track levels. Flag any important track that drops.

### 4. Dynamics
- Compress a track only when its loud moments jump out or its quiet details vanish. Begin with about
  6 dB of reduction on peaks, match the level with manual makeup gain, then ease the threshold back to
  the least that still holds the level steady (MS ch9).
- Gentle levelling: around 2:1, up to ~3:1. Isolated spikes: much higher ratio with the threshold just
  over the body of the signal. Trust the gain-reduction reading, not ratio or time labels, since they
  differ between units (MS ch9).
- Attack and release decide what gets evened out. Very fast settings on kick or bass (attack under
  ~50 ms) can thin the low end (MS ch9).
- Parallel compression for drums, piano, acoustic guitar. Judge it only blended, set from a group
  fader, and confirm delay compensation (MS ch9).
- Expansion goes before compression. Missing gate, transient shaper or ducker? Use volume envelopes
  written from item peaks or the beat grid (MS ch10, ch14, agent workaround).

### 5. EQ
- EQ exists here to fix balance and masking, judged with the full mix playing. Use solo only to hear
  what a band does (MS ch11).
- No presets, no copying last song's curve. Masking is specific to this arrangement (MS ch11).
- In a clash, the lower-ranked track gives way (`mix_masking` with `prefer=` the higher-ranked track,
  then `mix_compare`) (MS ch11).
- Shelves before peaks, peaks before notches; widest bandwidth that works; prefer cuts plus fader.
  More than one boosting band, or any boost beyond ~6 dB, is a warning sign (MS ch11).
- Five minutes on one track's EQ without a stable fader means the tool is wrong: try distortion for
  missing harmonics, a side chain, or dynamic/multiband treatment of the one region at fault
  (MS ch11–13).
- Bass often needs upper-mid content to survive dense guitars (MS ch11).
- Sibilance and moving resonances: dynamic treatment (soothe on the offending band), placed late in the
  vocal chain but before any saturator or exciter (MS ch13).

### 6. Reverb and delay
- Each reverb does one job. Typical set: a short natural **blend** reverb (10–20 ms predelay, low mids
  cut around 200–300 Hz, top rolled off), a longer **size** reverb (50 ms+ or tempo-based predelay,
  high-passed), and, on a few tracks during balancing, short coloured **tone/sustain** reverbs (MS ch16).
- Returns 100% wet, sends post-fader. EQ and compress the return, not the dry track (MS ch16).
- Delays smear less than reverb. Blend delay is one slapback of 50–100 ms with little feedback. Tone
  delays sit under 20 ms. Sync size/sustain delays to tempo: quarter note in ms = 60000 / BPM, set on
  or slightly behind the beat (MS ch17).
- Duck long effect returns from the lead vocal (sidechain if available, otherwise automation) (MS ch14,
  ch17).
- Don't add reverb by habit; modern pop and electronic parts are often meant to be dry and close
  (MS ch16).

### 7. Master bus and automation
- Put the buss compressor on during mixing. Glue means a few dB of reduction (about 2–3), ratio 2:1 or
  lower, slow attack, auto release, level-matched on bypass. No multiband on the master while mixing;
  loudness is mastering's job (MS ch19).
- Build lifts with arrangement, effects and width rather than level, because level gains are flattened
  by loudness processing (MS ch19).
- Automate per section first, then ride details: momentary masking, fills between vocal lines, line
  endings and weak consonants of the lead vocal. Small rides (fractions of a dB) matter; brief rides of
  6 dB or so are normal (MS ch19).

### 8. Referencing and delivery
- Compare against several commercial tracks of the same genre and era, lossless, same song section,
  gain-matched before judging, since louder always seems better (MS ch4, ch19).
  `mixcheck.py mix.wav --ref ref.wav` after matching RMS; note mixcheck has no LUFS and stops at 16 kHz.
- Keep a separate reference per question (low end, vocal, reverb, overall tone), plus a too-bright and
  a too-dark extreme to bracket the mix (MS ch4).
- Repeat render → compare → revise many times. Approve in a separate fresh pass with a written snag
  list, never at the end of a long tweak loop (MS ch4, ch19).
- Deliver the main mix without any temporary loudness processing, plus vocal-up, instrumental,
  a capella and stems, rendered with the buss compressor bypassed where the parts must sum back to the
  mix. Null-test the sum (MS ch4).
### Known gaps in our tooling (from the book's checks)
No mono/correlation or width meter; no LUFS; ~1/3-octave resolution only; no time-resolved band
levels; gain-reduction readout and compressor sidechain support unverified; no gate, transient shaper,
de-esser or pitch shifter in the plugin set.
