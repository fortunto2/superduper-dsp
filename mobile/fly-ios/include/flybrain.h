// C ABI for the connectome simulation running inside Flykeeper.
//
// Threading: create/destroy/load on the main thread. `fly_step` is called from the render
// loop. No call here allocates after `fly_create`, so the step is safe to run at frame rate.
#ifndef FLYBRAIN_H
#define FLYBRAIN_H
#include <stdint.h>

typedef struct FlyBrain FlyBrain;

// Lifecycle. `fly_create_synthetic` exists so the app is runnable before the real export is
// licensed and shipped; it reports itself as synthetic through fly_is_synthetic so no screen
// can present random wiring as a fly.
FlyBrain *fly_create_synthetic(uint32_t neurons, uint32_t fan_out, uint64_t seed);
FlyBrain *fly_create_from_csv(const char *path, uint32_t threshold, uint64_t seed);
// The app's packed export (brain.fcb from scripts/flywire-import.py): graph plus cell
// positions, edges under `threshold` synapses dropped at load. Null on any failure.
FlyBrain *fly_create_from_fcb(const char *path, uint32_t threshold, uint64_t seed);
void      fly_destroy(FlyBrain *brain);
int32_t   fly_is_synthetic(const FlyBrain *brain);

// Topology, for the UI to state what it is actually running rather than a tier label.
uint32_t fly_neurons(const FlyBrain *brain);
uint32_t fly_edges(const FlyBrain *brain);

// Simulation. fly_step advances one dt and returns the number of neurons that fired.
uint32_t fly_step(FlyBrain *brain);
uint64_t fly_total_spikes(const FlyBrain *brain);
uint64_t fly_steps(const FlyBrain *brain);
void     fly_reset(FlyBrain *brain, uint64_t seed);

// Sensory input: inject current into one neuron, or clear every injection. Clearing does
// not touch the background drive below, so a brain stays alive after a touch ends.
void fly_set_stimulus(FlyBrain *brain, uint32_t neuron, float current);
void fly_clear_stimulus(FlyBrain *brain);

// Many cells at once: the eye drives 10600 photoreceptors a frame, and one call per cell is
// most of that frame. `neurons` and `currents` are parallel arrays of `count` entries.
void fly_set_stimulus_many(FlyBrain *brain, const uint32_t *neurons, const float *currents,
                           uint32_t count);

// Background drive, the arousal dial. On the synthetic graph: 1.0 silent (sleep), 2.0 about
// 1% of cells firing a step, 3.5 about 5% (the default), 8.0 about 8%. A silent brain
// restarts on its own when raised; a touch wakes it regardless.
void  fly_set_noise(FlyBrain *brain, float noise);
float fly_noise(const FlyBrain *brain);

// Readout. Copies up to `capacity` indices of neurons that fired on the last step into
// `out`, returning how many were written. Copying rather than lending a pointer keeps the
// Rust side free to reuse its buffer on the next step.
uint32_t fly_copy_spikes(const FlyBrain *brain, uint32_t *out, uint32_t capacity);

// Neuron ids by index (an export's root ids; 0..n for a synthetic brain), so a cell can be
// placed at its real coordinates. Same copy-out contract.
uint32_t fly_copy_ids(const FlyBrain *brain, uint64_t *out, uint32_t capacity);

// Cell positions in µm as x,y,z triples in index order; `capacity` counts floats. 0 for a
// synthetic brain, which has no anatomy.
uint32_t fly_copy_positions(const FlyBrain *brain, float *out, uint32_t capacity);

// Running firing rate per cell (0..1, ~100 ms window), index order: the heat map.
uint32_t fly_copy_rates(const FlyBrain *brain, float *out, uint32_t capacity);

// Membrane voltages, for envelopes and for drawing. Same copy-out contract.
uint32_t fly_copy_voltages(const FlyBrain *brain, float *out, uint32_t capacity);

#endif
