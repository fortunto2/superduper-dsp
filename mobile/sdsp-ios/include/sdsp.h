// C ABI for the SuperDuper in-app synth (live2play). Rendered by an AVAudioSourceNode.
#ifndef SDSP_H
#define SDSP_H
#include <stdint.h>

typedef struct Engine SDSPEngine;

// Lifecycle (main thread).
SDSPEngine *sdsp_create(float sample_rate);
void        sdsp_destroy(SDSPEngine *engine);

// Control (main thread). param id: 0 cutoff · 1 resonance · 2 drive · 3 mod (each 0..1).
void sdsp_note_on(SDSPEngine *engine, uint8_t key, float velocity);
void sdsp_note_off(SDSPEngine *engine, uint8_t key);
void sdsp_all_notes_off(SDSPEngine *engine);
void sdsp_set_param(SDSPEngine *engine, uint32_t id, float value);

// Instrument engine (main thread). id: 0 Pad · 1 Wave · 2 Kubyz · 3 Drum (notes→drums).
void sdsp_set_instrument(SDSPEngine *engine, uint32_t id);

// Built-in timbre preset for the active instrument (main thread). Wave: 0 Saw·1 Square·2 Triangle·
// 3 Pulse·4 Morph. Kubyz: 0 Bashkir·1 Khomus·2 Real-D2.
void sdsp_set_instr_preset(SDSPEngine *engine, uint32_t id);

// Kubyz drone (main thread). root = MIDI note (tonality root); fixed = 1 drone on one pitch / 0 piano.
void sdsp_set_kubyz(SDSPEngine *engine, uint8_t root, uint32_t fixed);

// Instrument-specific param (main thread). idx 0/1, meaning depends on the active instrument:
// Wave 0 Sub / 1 Noise · Kubyz 0 Vowel · Drum 0 Tune / 1 Decay.
void sdsp_set_instr_param(SDSPEngine *engine, uint32_t idx, float value);

// FX chain (main thread). 3 slots in series. effect id: 0 off · 1 reverb · 2 filter · 3 saturator ·
// 4 delay · 5 chorus · 6 compressor. Per-slot params (idx 0..2) meaning depends on the effect.
void sdsp_set_effect(SDSPEngine *engine, uint32_t slot, uint32_t id);
void sdsp_set_effect_param(SDSPEngine *engine, uint32_t slot, uint32_t idx, float value);

// Render `frames` stereo samples (AUDIO THREAD ONLY — no allocation).
void sdsp_process(SDSPEngine *engine, float *out_l, float *out_r, uint32_t frames);

#endif
