# Optimising chain parameters against a measured loss (`tools/mixopt.py`)

The renderer is deterministic and a render costs ~0.1 s, so a mix is a function
and a bounded Nelder–Mead over three or four parameters beats guessing: each
step measured, every evaluation logged, nothing moved that the recipe did not
allow. Local search, not evolution: with a fast, exact evaluator a population
only spends renders on points a simplex already rules out.

## When it applies, and when it does not

Only technical questions with a measurable answer: band balance against a
reference, masking between two stems, loudness against a target, a bell's
frequency and depth once the two parts that fight are known. Not taste (a mix
closer to the reference by the numbers can sound worse) and not arrangement.
The method order holds (SKILL.md, "register first"): voicing, then timing,
then EQ; the optimiser is the EQ step, run after the first two are settled.

## The whitelist rule

`--vary` is the whole search space, and it is a whitelist you write, not one
the tool infers. Allowed: the parameters the recipe names for that stage (a
bell's `Mid Freq`/`Mid Gain`/`Mid Q`, an `HP`, a track `gain_db`, a master
`Threshold`), with the bounds the recipe would accept by hand. Never `Preset`,
a Mode, or a parameter on a plugin the recipe did not place; never a wide range
"to see what happens". Bounds outside the plugin's range are clipped, and said.

## Command

```
python3 tools/mixopt.py mix.toml \
  --vary "gtr.eq.Mid Gain:-12:0" "gtr.eq.Mid Freq:150:600" "gtr.eq.Mid Q:0.5:3" \
  --loss "ref:ref.wav,masking,loudness,level" --iters 60 --out best.toml
```

Loss terms, `=weight` optional (default 1): `ref:<wav or mixcheck --json>` is
the band-balance distance to the reference, dB RMS over mixcheck's bands with
each profile centred so level does not count; `loudness` is |LUFS gap| to the
reference; `masking` is Σ over stem pairs of Σ over the 32 bus bands of the
share of frames where both are > −50 dB and within 6 dB (the rule in
`synth-core/src/mixmap.rs`; 1.0 = one pair clashing in one band all the time);
`level` charges a stem whose broadband level (median over the bus bands that
carry energy) drops more than 2 dB against the base: a fader or wide shelf
pays, a narrow bell does not, so a part cannot win by getting quieter. Stems
render one track at a time, pre-master, with the tracks it keys off kept as
muted keys. `--span`/`--ref-span` compare chorus with chorus; `--iters 0`
measures the base and stops; `--resume` continues from the best point in
`<out>.jsonl`; `eq#2` is the second eq on a track, `master.limiter.Threshold`
the master chain. Output: `best.toml` (comments are not kept; diff it against
the base), its render `best.wav`, and `best.jsonl`, one line per evaluation
with the parameter diff and every term.

## Known-answer controls (`tools/test_mixopt.py`)

They skip, saying so, when `sdsp-chain` is not built. Pinned: identity (the
base against its own render) has loss 0 and twenty evaluations leave the
parameter alone; a planted +8 dB bell at 300 Hz on a chord stem against the
un-boosted mix is undone to ≤ +2 dB within 40 evaluations with the loss down
≥ 70 %; two stems overlapping at 300 Hz clash > 50 % of the time in the
266–331 Hz band and < 20 % after a −10 dB `Mid Gain`; a 5 dB fader drop costs
3.0 in the level term, a −10 dB Q 3 bell costs 0; the bus meter reads a −20 dBFS
tone as −20 dB. Measured 2026-10-21 on 4 s synthetic stems: +8 → −0.00 dB, loss
3.08 → 0.0004 in exactly 40 evaluations, 0.14 s each (0.28 s with two stems).

## Verify, then listen

A green loss is one meter agreeing with one reference. Before keeping
`best.toml`: run `mixverify` (or `mixcheck.py best.wav --ref ref.wav`) so a
meter that did not drive the search reads balance, crest and mono, and listen
to `best.wav` against the base. Masking is a threshold rule, flat until a
boundary is crossed: pair it with `ref:` or `level`, or widen `--step`.
