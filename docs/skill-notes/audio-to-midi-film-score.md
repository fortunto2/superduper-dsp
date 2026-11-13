# audio-to-midi: deriving a film theme from live playing (author's notes)

Moved out of the plugin: it uses `score_gen.py`, which is not part of it.

## В киномузыку, а не только в инструменты

Тема фильма может быть выведена из того, что человек реально сыграл в кадре:

```bash
python3 scripts/pitch_line.py piano.wav --theme      # → --chords "Am,Am,Cm,..."
python3 scripts/score_gen.py --plan film.otio --out score/ \
    --key A --chords "Am,Am,Cm,Dm,Bm,Dm,Am,E" --bpm 92,104,96 \
    --silence "1.0-3.6"          # там, где играет живое, партитуры нет
```

`--silence` делает **дырку** в партитуре, а не убавляет громкость: внутри окна
−91 дБ против −13.6 в полную силу. Приседающая под живой инструмент музыка
слышна, дырка — нет.

