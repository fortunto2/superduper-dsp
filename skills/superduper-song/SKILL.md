---
name: superduper-song
description: Make songs/tracks in REAPER driven entirely by our own SuperDuper DSP plugins (Wave, Kubyz, Drum, Pad, Sampler, Ambient + our FX) via the reaper MCP. Covers the exact param maps by name, preset-recall indices, the raw-value gotcha, CC expression maps, VST3i-vs-CLAPi, and audio→MIDI melody extraction. Use when the user says "сделай трек/песню на наших плагинах", "собери бит на Drum", "мелодию на Wave/Kubyz", "поставь пресет Aulos", "покрути параметры удалённо", "переложи мелодию из аудио в миди". Living skill — grow the maps + recipes each session.
---

# superduper-song — сочинять треки нашими плагинами в REAPER

Наши 23 CLAP-плагина существуют ровно для этого: агент/DAW рулит ими headless через **reaper MCP** (`total-reaper-mcp`). Всё, что экспонировано как CLAP-параметр, крутится удалённо — отдельный MCP под плагин НЕ нужен, reaper MCP уже универсальный (адресует по `track_index, fx_index, param_index`).

Общая механика REAPER-MCP (треки/MIDI/маркеры/рендер/мост-IPC) — в скилле **`reaper-daw`**. Здесь — только то, что про НАШИ инструменты и сборку песни ими.

## ⚠️ Критично: `track_fx_set_param` берёт СЫРОЕ значение, не 0..1

Проверено 2026-07-24 живьём. `track_fx_get_param` возвращает `value (range: min to max)` — в реальных единицах. `set_param` пишет туда же **в тех же единицах**:
- Cutoff (Гц): `set_param(param=4, value=1800)` — 1800 Гц, НЕ 0.18.
- Preset (индекс): `set_param(param=37, value=14)` — пресет №14, НЕ «нормализ. 1.0» (это дало бы индекс 1).
- Уровни/микс 0..1 → так и пиши 0..1; dB → в dB; ST → полутона.

(Старый `reaper-daw` писал «params normalized 0..1» — это неверно для этого моста, поправлено там же.)

Всегда сверяйся: `track_fx_get_param(track, fx, idx)` печатает и значение, и диапазон — бери его как истину.

## Плагин = VST3i или CLAPi (важно после пересборки)

`track_fx_add_by_name "SuperDuper Wave"` фаззи-матчит и ставит **VST3i** (обёртка). Обычно ок — тот же код. НО:
- Display-имя несёт build-номер: `SuperDuper Wave [bNNNNN]`. Свежесобранный бинарь = БОЛЬШОЙ номер.
- После пересборки плагина REAPER продолжает держать СТАРЫЙ бинарь в уже открытых инстансах; **rescan (Prefs→Plug-ins→CLAP→Clear cache+rescan) регистрирует новый .clap, но НЕ заменяет загруженные инстансы.**
- Чтобы гарантированно взять свежий код — добавляй с префиксом **`CLAPi: SuperDuper <Name>`** (грузит .clap напрямую). Проверь `track_fx_get_name` — build-номер должен совпасть со свежим бинарём (`strings <bundle>/Contents/MacOS/<Name> | grep -oE "\[b[0-9]+\]"`).
- Инструмент ДОЛЖЕН быть FX 0 (до эффектов). `add_by_name` кладёт в конец → чтобы переставить в 0 без потери цепочки: `track_fx_copy_to_track(source=T, fx_index=последний, dest_track=T, dest_fx_index=0, move=true)`.

## Инструменты и карты параметров (индекс → имя, диапазон)

Все — MIDI-инструменты (нота-in), кроме Ambient (генератор). У каждого последний параметр — **Preset** (stepped, recall всего тембра/кита одним write'ом, значение = СЫРОЙ индекс).

### SuperDuper Wave — wavetable синт (P_PRESET=37)
`0 WT Pos(0..1) · 1 Unison(1..7) · 2 Detune(0..50ct) · 3 Sub(0..1) · 4 Cutoff(30..18000Hz) · 5 Resonance(0..0.9) · 6 Filter(0..2 mode) · 7 Drive(0..1) · 8 Attack(0.001..4s) · 9 Decay(0.01..4s) · 10 Sustain(0..1) · 11 Release(0.01..8s) · 12 Output(-36..6dB) · 13 Anti-Alias · 14 Noise(0..1) · 15-19 FEnv(Amt/A/D/S/R) · 20 LFO Rate(0.05..30Hz) · 21 LFO Depth · 22 LFO Shape(0..3) · 23 LFO Dest(0..2) · 24 Bend Range(ST) · 25-26 LFO Sync/Div · 27-32 Mod1/Mod2(Src/Dst/Amt) · 33-34 Sync/Ratio · 35-36 FM Ratio/Amt · 37 Preset`
Пресеты (индекс): `0 Init(Sine) · 1 Saw · 2 Square · 3 Triangle · 4 Pulse25 · 5 Sine→Saw · 6 Sine→Square · 7 Saw→Square · 8 Triangle→Saw · 9 Reese Bass · 10 FM Growl · 11 808 Sub · 12 Fat Saw Lead · 13 Formant · 14 Aulos(Odyssey)`
Форма волны = часть пресета → reese/808/formant/aulos только через Preset-param (или GUI), не отдельным параметром.

### SuperDuper Kubyz — physical-model варган/хомус (P_PRESET=19)
`0 F1(80..1500Hz) · 1 F2(200..3500) · 2 F3(600..6000) · 3 VoxMix(0..1) · 4 Vel Shift(0..0.5) · 5 Bright(0..2) · 6 Attack(0.001..2s) · 7 Decay(0.01..4s) · 8 Sustain(0..1) · 9 Release(0.01..4s) · 10 Output(dB) · 11 Tongue ST(-36..36) · 12 Mouth Shp(0..4) · 13 Mouth Rate(0.05..20Hz) · 14 Mouth Dep · 15 Mouth Stereo · 16 Bend Range · 17-18 M Sync/Div · 19 Preset`
Пресеты: `0 Init(sine) · 1 Bashkir Kubyz · 2 Khomus Sample · 3 Real D2 · 4 Aulos(Odyssey)`. Тон башкирского кубыза A#2, якутского F#2, татарского C#2 (см. `dj-set-dramaturgy-kubyz`).

### SuperDuper Drum — 6 синтезируемых голосов (P_PRESET=27)
Голоса Kick/Snare/HHc/HHo/Clap/Cowb — каждый 4 параметра `Tune(ST) · Decay(s) · Level(0..1) · Pan(-1..1)`, индексы 0..23 (голос×4+поле). Затем `24 Drive · 25 Master(dB) · 26 Note Out · 27 Preset`. GM-ноты 35-57 (Kick=36, Snare=38, HHc=42, HHo=46, Clap=39, Cowb=56). Пресеты: Trap/Boom Bap/808 Sub/Techno/Joy Division/Boards of Canada. Ноты вне карты форвардятся на CLAP note-out (роутить в Wave/Kubyz).

### SuperDuper Pad — полифонический синт (P_PRESET=14)
`0 Cutoff(80..16000Hz) · 1 Resonance(0..0.9) · 2 Modulation(0..50ct) · 3 Drive · 4 Width(0..30ct) · 5 Attack · 6 Decay · 7 Sustain · 8 Release · 9 Output(dB) · 10 Bend Range · 11 Env Delay · 12 Env Hold · 13 Polyphony(2..16) · 14 Preset`

### SuperDuper Sampler — WAV-плеер + YIN-тюнер (полифон)
`0 Sample(0..255) · 1 Root(0..127) · 2 Tune(ST) · 3 Fine(ct) · 4 Loop · 5-6 Loop Start/End · 7-10 ADSR · 11 Output · 12-13 Start/End · 14 Reverse · 15 Filter(0..4) · 16 Cutoff(0..127) · 17 Reso · 18 Env>Cutoff · 19 Vel>Amp · 20 Vel>Cut`. Сэмплы сканит `~/Music/SuperDuper Samples/` + `~/Music/Favorite 808s/`. Выбор WAV = параметр `Sample` (индекс в отсканированном списке).

### SuperDuper Ambient — автономный дрон-генератор (без MIDI). Просто добавить на трек, играет сам.

## Пресет-recall механика (весь тембр одним write'ом)

`set_param(Preset_idx, СЫРОЙ_индекс)` — переключает пресет со всем: волна/кит/формант/огибающая. Но:
- **Recall применяется надёжно ТОЛЬКО под воспроизведением** (`dsl_play` до/во время). На idle-треке событие может не долететь до плагина. Порядок: `dsl_play()` → `set_param(Preset, idx)` → проверить.
- Проверка что применилось: прочитай параметр, который пресет переопределяет (напр. Wave Aulos → Unison должен стать 3, Cutoff 1800, Detune 12). Если дефолт — recall НЕ прошёл (частая причина: idle-трек, или значение дали нормализованным 1.0 вместо индекса).
- `apply_preset` метит все параметры dirty → хост/автоматизация видит recall.

## Экспрессия в реальном времени (CC)

Наши синты слушают MIDI CC для живой игры (CC пишутся в MIDI-клип, НЕ поднимают dirty → нет feedback-петли):
- **CC7 (Volume) → выходной VCA** на Wave и Kubyz (добавлено 2026-07-24) — протяжный breath/swell громкостью, нейтрально (1.0) пока не придёт. Совпадает с gesture-картой live2play (off-hand→vol). См. `wave-kubyz-cc7-vca-aulos`.
- Wave: CC1→LFO Depth, CC11→Cutoff, CC71→Resonance, CC74→WT Pos, aftertouch→LFO Depth, pitchbend→±Bend Range.
- Kubyz: CC1→Mouth Depth, CC2→Mouth Stereo, CC11→F1, CC71→F3, CC74→F2.
- Pad/Wave/Kubyz: pitch-bend + MIDI CC + tempo-sync LFO (host BPM из Transport-события).
Огибающая (Attack/Release) — для духового «дыхания» ноты, если CC-swell не гоняешь: медленный Attack = нота набегает, медленный Release = угасает.

## Сборка песни — типовой поток

1. **Темп:** `dsl_set_tempo(bpm)`. Приём: `bpm=60` → 1 доля = 1 секунда, MIDI `start/length` в секундах читаются как секунды таймлайна (удобно). Проверить реальный темп: `Master_GetTempo` через мост (dsl_get_tempo_info врёт 120).
2. **Треки:** `dsl_track_create(name, role)`. ⚠️ **PREPEND** — каждый новый трек в индекс 0, порядок инвертируется. Создавай в обратном порядке ИЛИ переименовывай по индексам после. (`reaper-daw` gotcha-таблица.)
3. **Инструмент:** `track_fx_add_by_name(track_idx, "SuperDuper Wave")` (или `CLAPi: ...` для свежего). Должен быть FX 0.
4. **Тембр:** либо пресет одним recall'ом (`set_param(Preset, idx)` под play), либо параметры по одному по карте выше. Без пресета можно ВСЁ, что непрерывный параметр; форма волны/кит/формант — только пресетом.
5. **MIDI:** `dsl_midi_insert(track, time={"start":s,"end":s}, midi_data={notes:[{pitch,start,length,velocity}]})`. `start/length` — СЕКУНДЫ от начала item. Много нот → мост-IPC (CreateMIDIItem+InsertMIDINote loop, слоты 700+), не инлайнить тысячи в аргумент.
6. **FX-цепочка (наши эффекты):** после инструмента — `track_fx_add_by_name(track, "SuperDuper Reverb")` и т.д. Наш арсенал: Eq, LinEq, Compressor, Saturator, Limiter, MidSide, Reverb, Supermass, Delay, Chorus, Filter, Soothe, Vocal, NAM, Spectrum. Порядок = порядок добавления; инструмент держать в 0.
7. **Аранжировка:** маркеры/регионы `dsl_marker`/`dsl_region` (секунды или "16 bars").
8. **Мастер:** in-REAPER цепочка на мастере, ИЛИ headless через скилл `sdsp-chain` (EQ→Comp→Sat→Limiter, per-stage LUFS/dBTP). Метр — наш Spectrum-плагин (LUFS/TP).

## Приём: аудио → MIDI (переложить мелодию из записи)

Как делали для темы Odysseus (Göransson). Вытащить ноты из аудио и вставить как MIDI под наш инструмент:
1. `pyin` (librosa) по нужному куску: `f0 = librosa.pyin(y, fmin, fmax, sr, frame_length=4096, hop_length=512)`. Узкий регистр (fmin/fmax) убирает октавные скачки детектора.
2. Класс высоты через `chroma_cqt` (salience по нотам) — снимает октавную неоднозначность, находит тонику.
3. Сегментировать в устойчивые ноты (соседние voiced-кадры в пределах ~полутона, длина ≥0.3-0.5с).
4. Velocity = пик RMS каждой ноты (`librosa.feature.rms`, нормировать на 99-й перцентиль).
5. Вставить `dsl_midi_insert` на bpm=60 (start=сек). ⚠️ микротон живых/этно-инструментов (авлос, кубыз) → ET-квантизация рядом, финально подстроить питч инструмента на слух.
6. Спектр оригинала (`np.fft.rfft`, гармоники относительно фундаментала + центроид + биения) → перевести в параметры синта (детюн/unison под биения, cutoff под центроид, drive под богатство обертонов, noise под придыхание). Так строился пресет Aulos.
venv: `/opt/homebrew/bin/python3.13 -m venv` + `pip install numpy librosa mido` (НЕ `~/.local/bin/python` — Pyodide).

## Гоча-сводка (быстрый чек)

| Симптом | Причина | Лечение |
|---|---|---|
| Пресет/параметр не меняет звук | value дан нормализованным, а нужен СЫРОЙ индекс/единица | Preset=14 (не 1.0), Cutoff=1800 (не 0.18) |
| Пресет-recall не применился | idle-трек | `dsl_play` → потом set Preset; проверить перезаписанный параметр |
| Плагин со старым звуком после пересборки | REAPER держит старый бинарь | добавить как `CLAPi:` заново; сверить build-номер в имени |
| Инструмент звучит после эффектов | add кладёт в конец | `copy_to_track(move=true, dest_fx_index=0)` |
| Треки в обратном порядке | dsl_track_create prepend'ит | создавать в обратном порядке / переименовать по индексам |

## Related
- `reaper-daw` — общая механика REAPER-MCP (треки/MIDI/мост-IPC/рендер).
- `sdsp-chain` — headless мастеринг наших эффектов + LUFS/TP.
- `sdsp-mash` — мэшапы/сайферы.
- `superduper-plugin` — создавать/править сами плагины.
- `serum-synth` — если вдруг Serum, не наши.
- Память: `wave-kubyz-cc7-vca-aulos`, `dj-set-dramaturgy-kubyz`, `live2play-default-gesture-map`.
