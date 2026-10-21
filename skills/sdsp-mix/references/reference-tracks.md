<!-- Reference-track library. Track picks marked [A] come from Tom Frampton, "Best Reference
     Track For All Genres", Mastering The Mix (13 Jan 2025):
     https://www.masteringthemix.com/blogs/learn/best-reference-track-for-all-genres
     The "use it for" notes are our paraphrase, not the article's text. Picks marked [+] were
     added here and checked to exist (title, artist, year) on 2026-10-21. No audio lives in
     this repo, ever. -->
## Reference tracks

A reference answers one question at a time. "Does my mix sound like a record?" is not a question
`mixcheck` can answer; "is my sub as loud as theirs, relative to the mids, at the same loudness?" is.
So every track below is tagged with what it is good for, not just with a genre.

Tags: **low** (sub, kick/bass relationship, mono low end) · **vocal** (level, presence, de-essing) ·
**space** (width, depth, reverb) · **bright** (top-end tilt) · **dyn** (crest factor, transients) ·
**loud** (how far the master is pushed). **⇡ bright / ⇣ dark** marks a bracket candidate.

### How to use

1. **Get them lossless.** Buy from Bandcamp, Qobuz or 7digital (FLAC/WAV), or rip a CD you own.
   Never rip a stream: lossy codecs cut the top band and smear transients, which is exactly what
   the `top`, `air` and crest readings measure. A bad reference is worse than none.
2. **Store them outside every repo**: `~/Music/references/<genre>/Artist - Title.flac`.
   Never commit audio. The manifest (`reference-tracks.example.toml`, copy it to
   `~/Music/references/references.toml`) holds the paths and the section times.
3. **Compare the same section.** Chorus against chorus, drop against drop. A verse has a different
   band balance and loudness, and the comparison means nothing. `mixcheck` cuts both sides:
   `mixcheck mix.wav --start 70 --end 100 --ref ref.wav --ref-start 62 --ref-end 92`;
   `mixcheck ref.wav --start 62 --end 92 --json > ref-chorus.json` stores a section's profile.
   mixcheck reads WAV only: a FLAC goes through
   `ffmpeg -i ref.flac -c:a pcm_s24le /tmp/ref.wav` first (also the input Demucs wants).
4. **Loudness-match before judging.** `mixcheck mix.wav --ref ref.wav` prints
   `reference … LUFS — this mix is +x.x LU`. Louder always sounds better, so if the gap is over
   1 LU, turn the louder one down for listening. The band table is already level-relative, so read
   it without matching, but never A/B by ear until the LU gap is under 1.
5. **Several references, one question each.** Two or three tracks of the same genre and era,
   chosen per question (low end from one, vocal from another), beat one "perfect" reference.
6. **Bracket the tone.** Pick one reference brighter than you want and one darker. If the mix's
   `air`/`top` sits between them it is in range; outside both, it is the mix, not the speakers.
   For the [+] tracks the bracket role is a guess until measured: run `mixcheck` once on each
   and write the `air`/`top` values into the manifest.

**Which numbers answer which question:**

| Question | Compare in `mixcheck` | Problem looks like |
|---|---|---|
| low end | `sub`, `kick` band deltas; `side/mid under 120 Hz`; `mono loss` on sub/kick | sub ±4 dB off the ref; side/mid above −12 dB (wide bass) |
| vocal | `mid` and `presence` deltas, on a section where the vocal leads | presence −4 dB = buried vocal; +4 dB = harsh |
| bright | `air` and `top` deltas against both bracket refs | below the dark ref = dull; above the bright ref = harsh |
| dyn | `crest`; gap between integrated LUFS and loudest-3 s LUFS | crest < 8 dB while the ref is 10+ = over-compressed |
| loud | integrated LUFS vs the ref | a gap you close in mastering, not the mix |
| space | `correlation`; `mono loss` on body/mid/presence | correlation well below the ref's = too wide/phasey |

Body and low-mid buildup (`low`, `body`) is the most common deviation in home mixes; check it
against every reference, whatever the question.

### Artists the owner already references

The owner already checks mixes against these three, so they get their own per-question split.
All [+], verified.

| Track | Use it for |
|---|---|
| Eminem — Lose Yourself (2002) | dense, fast rap vocal that stays intelligible over band-style drums and guitar · vocal, dyn |
| Eminem — Without Me (2002) | dry, centred vocal on a bouncy, mono-solid beat · vocal, low |
| Eminem — Rap God (2013) | very fast delivery: consonant clarity, presence band under load · vocal |
| Travis Scott — SICKO MODE (2018) | see hip-hop: 808 and beat switches · low, space |
| Travis Scott feat. Kendrick Lamar — goosebumps (2016) | wet, effected vocal over 808; reverb/delay as part of the sound · space, low |
| Travis Scott — HIGHEST IN THE ROOM (2019) | dark, washed vocal over a heavy sub · low · ⇣ dark |
| Travis Scott & Playboi Carti — FE!N (2023) | hard-limited modern trap master · loud, low |
| Twenty One Pilots — Stressed Out (2015) | hip-hop drums and bass under a pop/alt vocal · low, vocal |
| Twenty One Pilots — Heathens (2016) | dark, sparse mix with a low vocal and big low end · low, space |
| Twenty One Pilots — Chlorine (2018) | layered vocals and wide synths over a soft beat · space, vocal |

### Pop

| Track | Src | Use it for |
|---|---|---|
| Dua Lipa — Don't Start Now (2019) | A | vocal production, tight drums/synth groove · ⇡ bright |
| Zedd, Maren Morris, Grey — The Middle (2018) | A | loud master that keeps drum punch; bass with mid presence · loud, dyn |
| Drake — One Dance (2016) | A | classic 808 under a pop vocal · low |
| Luis Fonsi & Daddy Yankee — Despacito (2017) | A | acoustic guitar + percussion in a modern pop mix; backing-vocal panning · space |
| John Legend — Penthouse Floor (2016) | A | warm low-mids, controlled sub, soft hi-hats · ⇣ dark |
| Billie Eilish — bad guy (2019) | + | huge sub under a near-whispered vocal; sparse, open master · low, vocal, dyn |

### Hip-hop / trap with 808

| Track | Src | Use it for |
|---|---|---|
| Arizona Zervas — Roxanne (2019) | A | kick vs 808 relationship, crisp vocal (watch the rumble under 40 Hz) · low, vocal |
| 50 Cent — In Da Club (2003) | A | tight bass, drums leading the bass, darker top · low · ⇣ dark |
| Dr. Dre — Still D.R.E. (1999) | A | clean vocal, strong mono compatibility · vocal, space |
| Travis Scott — SICKO MODE (2018) | + | 808 slides and beat switches; dense, wide production · low, space |
| Kendrick Lamar — HUMBLE. (2017) | + | dry, upfront rap vocal over piano and 808 · vocal, low |
| Metro Boomin, The Weeknd, 21 Savage — Creepin' (2022) | + | modern trap low end under a sung vocal · low, loud |

Cross-reference from the article: J Balvin — Mi Gente (2017): short kick tail fused with a big
808; compare the chorus, the verses are muddier.

### Russian-language pop

| Track | Src | Use it for |
|---|---|---|
| Zivert — Life (2018) | + | Russian dance-pop vocal on top of a steady four-on-the-floor · vocal, loud |
| Artik & Asti feat. Артём Качер — Грустный дэнс (2019) | + | two-vocal pop balance (female/male), radio master · vocal, loud |
| Miyagi & Andy Panda — Kosandra (2020) | + | 808 and reggae-tinged groove under Russian rap/sung vocal · low, vocal |
| Монеточка — Каждый раз (2018) | + | lighter, less-limited indie-pop master; vocal kept intimate · dyn, vocal |

Russian vocals carry many sibilants and hard consonants; compare `presence`/`air` against these
before judging a de-esser by ear.

### Folk / world with acoustic instruments

| Track | Src | Use it for |
|---|---|---|
| Ay Yola — Homay (2025) | + | Bashkir folktronica: kyl-kubyz, kurai, dombra over electronic low end. Closest genre match for kubyz folk-pop · low, space, vocal |
| Otyken — Storm (2022) | + | Siberian ethnic band with jaw harp and throat singing. Where a jaw harp sits against drums and vocal · space, vocal |
| The HU — Wolf Totem (2018) | + | Mongolian folk instruments and throat singing in a rock-weight mix · low, space |
| Wardruna — Helvegen (2013) | + | acoustic/traditional instruments with big natural space, wide dynamics · space, dyn |
| José González — Heartbeats (2003) | + | solo nylon guitar + vocal, close and dry · vocal, dyn |
| Bon Iver — Holocene (2011) | + | acoustic guitar and voice inside a deep reverb picture · space |
| Chris Stapleton — Broken Halo (2017) | A | acoustic guitar, warm vocal, balanced drums |

Jaw harp (kubyz, vargan) packs its energy into a narrow mid band with strong overtones; compare the
`mid`/`presence` deltas on a section where it plays, not across the whole song.

### Lo-fi / instrumental hip-hop

| Track | Src | Use it for |
|---|---|---|
| Nujabes — Aruarian Dance (2004) | + | soft, rolled-off top and warm mids; reference for "dark on purpose" · ⇣ dark, dyn |
| J Dilla — Time: The Donut of the Heart (2006) | + | sample-based groove, loose drums, gritty texture · dyn |
| Tom Misch — Movie (2018) | + | lo-fi-adjacent: clean guitar and soft drums, polished but gentle master · bright, dyn |

Lo-fi references are deliberately dark and soft. Use them only for a lo-fi mix; checked against a
pop mix they will say everything is too bright.

### Electronic

| Track | Src | Use it for |
|---|---|---|
| Matrix & Futurebound — Magnetic Eyes (2012) | A | DnB: deep sub, crisp synths, upfront vocal · low, vocal |
| Jack Ü — Where Are Ü Now (2015) | A | deep sub, sparse arrangement, big loudness range (true peaks above 0 dBTP, so not a peak reference) · low, dyn |
| Skrillex — Bangarang (2011) | A | dubstep: gritty full-spectrum sound (some over-limited moments) · loud |
| Zedd — Adrenaline (2014) | A | EDM: club low end, clear lead, punchy drums, no vocal · low, dyn |
| Audien — Wayfarer (year unverified) | A | trance: wide synths, uncluttered arrangement · space |
| Major Lazer — Lean On (2015) | A | lead/vocal focus, wide low-mid chords · vocal, space |

### House

| Track | Src | Use it for |
|---|---|---|
| CamelPhat & AuRa — Panic Room (year unverified) | A | deep house: clean kick and bass, focused vocal · low, vocal |
| Oliver Heldens — Gecko (2013) | A | huge sub balanced across the spectrum · low |
| Disclosure — F For You (2013) | A | reverb-based synth depth, lighter bass · space |
| DubVision — I Found Your Heart (2015) | A | bass harmonics that survive small speakers · low · ⇡ bright |
| deadmau5 — My Pet Coelacanth (year unverified) | A | contrast between breakdown and drop · dyn |

### R&B

| Track | Src | Use it for |
|---|---|---|
| Destiny's Child — Say My Name (1999) | A | uncluttered mix leaving room for vocals · vocal |
| DJ Khaled feat. Rihanna, Bryson Tiller — Wild Thoughts (2017) | A | consistent tonal balance (very loud, some distortion) · loud |
| Next — Too Close (1997) | A | deep bass balanced with kick, controlled top · low |

### Rock

| Track | Src | Use it for |
|---|---|---|
| Arctic Monkeys — D Is For Dangerous (2007) | A | breathing room, no harshness (mastered hot, peaks above 0 dBTP) · space |
| Imagine Dragons — Natural (2018) | A | bright-not-harsh vocal, rich chorus balance · vocal · ⇡ bright |
| Rage Against The Machine — Bombtrack (1992) | A | aggressive guitars, powerful kick and snare · dyn |
| Paramore — Misery Business (2007) | A | wide guitars, clear vocal, great snare (light bass) · space, vocal |
| Linkin Park — Numb (2003) | A | deep but restrained bass, clean drums, dense texture · low |

### Engineer showcases (genre-neutral tone checks)

| Track | Src | Use it for |
|---|---|---|
| Troye Sivan — My My My! (2018) | A | Serban Ghenea mix: clarity, rich low end, intact transients · low, dyn |
| Imagine Dragons — Only (year unverified) | A | Ghenea: modern synths and drums · ⇡ bright |
| Kendrick Lamar & SZA — All The Stars (2018) | A | MixedByAli: spectrum balance, detailed vocal · vocal · ⇣ dark |
| Dua Lipa — New Rules (2017) | A | Josh Gudwin: big bass, loud but punchy · low, loud |
| David Bowie — Lazarus (2016) | A | Tom Elmhirst: warm yet clear, punchy kick and bass · low · ⇣ dark |
| Miguel — Pineapple Skies (2017) | A | Jaycen Joshua: deep sub from bass and kick · low |

### Other genres from the article

K-pop: EXO — Ko Ko Bop (2017), punchy short kick, bright vocal. Reggaeton: J Balvin & Willy William —
Mi Gente (2017), see hip-hop above. Jazz: Esperanza Spalding — I Know You Know (2008), preserved
dynamics. Orchestral: Hans Zimmer — Time (2010), full spectrum around 3:00. Disco/Funk/Motown
(Michael Jackson, Stevie Wonder, Marvin Gaye) are useful for arrangement and panning, not for modern
low end. Metal: Tool — The Pot (2006).

### Auto-reference: tearing down a new reference

A whole-mix comparison says "your presence band is 4 dB low". It cannot say whether that is the
vocal, the guitar or the hats. Split the reference into stems and measure the vocal on its own.
Stems come from the `stem-split` skill (Demucs via uvx). Tempo, groove and arrangement come from
`track-teardown`, and notes from `audio-to-midi`. This section covers only the mix questions.

**Rule: put both sides through the same separation.** Demucs leaves its own artefacts and moves
part of the vocal reverb into `other`. Split our mix too and compare stem against stem
(`their vocals.wav` vs `our vocals.wav`), never their stem against our dry vocal track. The
separation error is then the same on both sides and drops out of the delta.

```bash
# 1. cut the same section from both, then split (two stems: vocals / no_vocals)
ffmpeg -ss 62 -to 92 -i ref.flac ref_chorus.wav
uvx --with numpy --from demucs demucs --two-stems=vocals -o stems ref_chorus.wav mix_chorus.wav
R=stems/htdemucs/ref_chorus; M=stems/htdemucs/mix_chorus

# 2. vocal EQ: the 8-band curve of the vocal alone, ours against theirs
python3 tools/mixcheck.py $M/vocals.wav --ref $R/vocals.wav

# 3. vocal level against the music (LU): run on vocals.wav and no_vocals.wav of each side
ffmpeg -hide_banner -nostats -i $R/vocals.wav -af ebur128 -f null - 2>&1 | grep -m1 'I:'

# 4. vocal pan: per-channel RMS (first line L, second R; the difference is the offset in dB)
ffmpeg -hide_banner -i $R/vocals.wav -af astats=measure_overall=none:measure_perchannel=RMS_level -f null - 2>&1 | grep RMS

# 5. vocal width / reverb: mid then side RMS of the vocal stem
ffmpeg -hide_banner -i $R/vocals.wav -af "pan=stereo|c0=0.5*c0+0.5*c1|c1=0.5*c0-0.5*c1,astats=measure_overall=none:measure_perchannel=RMS_level" -f null - 2>&1 | grep RMS
```

Commands 4 and 5 were checked on a known signal: a tone at 0 dB in L and −6 dB in R read 6.0 dB
L−R and side/mid −9.5 dB, the exact expected values.

How to read it:

| Question | Number | Reading |
|---|---|---|
| vocal EQ | step 2 band deltas, mainly `body`, `mid`, `presence`, `air` | ±4 dB on one band names the EQ move; the whole curve tilted means a shelf |
| vocal level | step 3: vocal LUFS minus no_vocals LUFS, theirs vs ours | ours 2 LU lower = vocal buried, whatever the band table says |
| pan | step 4: L−R | a lead vocal sits within ±0.5 dB; a larger offset in the reference is a doubled or panned layer |
| width / reverb | step 5: side − mid | compare to the reference, not to a fixed number. Rough starting guide (not yet measured on real stems): about −25 dB or lower = dry centred vocal, −15 to −10 dB = audible stereo reverb or doubles, above −8 dB = vocal spread wide |
| dynamics | step 2 `crest` on the vocal stem | ours 3 dB lower = vocal compressed harder than theirs |

Reverb *length* is not measured yet. Listen to `vocals.wav` at the end of a phrase: the tail
there is the reverb, and Demucs cuts part of it off, so it reads shorter than it is. The next tool to build
is a script that measures the decay after each phrase ends.

### Storing what a reference measures

Two files, two jobs. The **manifest** (TOML, hand-edited) says which tracks, which section, what
question. The **profile** (JSON, machine-written) caches what `mixcheck` measured for that section:
LUFS integrated and loudest 3 s, crest, correlation, the 8 band levels, mono loss, side/mid. JSON
because it is written and read by code and diffed per mix, never edited by hand; keep it next to the
audio, e.g. `~/Music/references/pop/Dua Lipa - Don't Start Now.chorus.json`. A chart of "my mix vs
the reference set" is then drawn from profiles, not from re-decoding audio every time.

**The corridor** (`--target`): `mixcheck mix.wav --target refs/trap/*.json` (or the directory)
turns 5–10 chorus profiles into the genre's tonal balance — per band the mean and spread of the
mean-centred profiles, so a −14 LUFS mix compares with −8 LUFS references by shape alone — and
says where the mix leaves it ("kick above by 2.3 dB", "air below by 1.1 dB"). Move EQ or
Multiband only on a band that is outside; inside the corridor is taste, not a fault. One
reference gives a corridor of ±1 dB (the floor), which is a comparison, not a law. `--json
--target …` adds `target.outside_db` per band for the agent and for `mixopt --loss band`.
