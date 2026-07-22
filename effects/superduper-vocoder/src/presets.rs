//! Factory presets for SuperDuper Vocoder.

use crate::dsp::{
    BANDS_11, BANDS_16, BANDS_20, PITCH_AUTO, PITCH_VOICE, SRC_SIDECHAIN, WAVE_PULSE, WAVE_SAW,
    WAVE_SAWSUB, WAVE_SQUARE,
};
use crate::{
    P_ATTACK, P_BANDS, P_DETUNE, P_DRIVE, P_FORMANT, P_MIX, P_PITCH, P_PITCH_SOURCE, P_RELEASE,
    P_SOURCE, P_UNVOICED, P_WAVE, PARAMS,
};

superduper_dsp_sdk::define_preset!(PARAMS);

pub static PRESETS: &[Preset] = &[
    Preset::from_overrides("Default", &[]),

    // Flagship. Internal saw, fast attack / moderate release, formant nudged
    // down for that heavier robot timbre, a touch of drive + unvoiced for
    // clarity, fully wet.
    Preset::from_overrides("Daft Punk Robot", &[
        (P_ATTACK, 2.5),
        (P_RELEASE, 22.0),
        (P_PITCH, 0.0),
        (P_DETUNE, 8.0),
        (P_FORMANT, -1.0),
        (P_UNVOICED, 0.18),
        (P_DRIVE, 0.35),
        (P_MIX, 1.0),
    ]),

    // Wide, choral. Saw+sub carrier for body, slow release so words smear
    // into a pad, no drive, generous unvoiced air.
    Preset::from_overrides("Kraftwerk Choir", &[
        (P_WAVE, WAVE_SAWSUB as f32),
        (P_ATTACK, 4.0),
        (P_RELEASE, 70.0),
        (P_DETUNE, 12.0),
        (P_FORMANT, 0.0),
        (P_UNVOICED, 0.25),
        (P_DRIVE, 0.0),
        (P_MIX, 1.0),
        (P_BANDS, BANDS_20 as f32), // intelligible, wide
    ]),

    // Reedy talkbox honk. Pulse carrier, tight envelope, slightly less than
    // fully wet so the source articulation still reads.
    Preset::from_overrides("Talkbox", &[
        (P_WAVE, WAVE_PULSE as f32),
        (P_ATTACK, 1.0),
        (P_RELEASE, 8.0),
        (P_DETUNE, 3.0),
        (P_FORMANT, 0.0),
        (P_UNVOICED, 0.10),
        (P_DRIVE, 0.15),
        (P_MIX, 0.9),
        (P_BANDS, BANDS_11 as f32), // tinny, honky
    ]),

    // Vocode any synth/pad routed to the sidechain input (port 1) with the
    // modulator on the main input. Neutral dynamics, no drive.
    Preset::from_overrides("Sidechain Synth", &[
        (P_SOURCE, SRC_SIDECHAIN as f32),
        (P_ATTACK, 4.0),
        (P_RELEASE, 40.0),
        (P_UNVOICED, 0.12),
        (P_DRIVE, 0.0),
        (P_MIX, 1.0),
    ]),

    // Live rig: play chords on a MIDI keyboard to pitch the robot voice, sing
    // into the audio input. Auto = keys when held, else pitch-tracks the voice.
    Preset::from_overrides("Live Keys", &[
        (P_PITCH_SOURCE, PITCH_AUTO as f32),
        (P_ATTACK, 3.0),
        (P_RELEASE, 25.0),
        (P_DETUNE, 10.0),
        (P_UNVOICED, 0.15),
        (P_DRIVE, 0.10),
        (P_MIX, 1.0),
    ]),

    // Thickener rather than full robot — low Dry/Wet so the vocoded layer
    // sits under the natural voice.
    Preset::from_overrides("Subtle Doubler", &[
        (P_ATTACK, 3.0),
        (P_RELEASE, 25.0),
        (P_DETUNE, 6.0),
        (P_UNVOICED, 0.10),
        (P_DRIVE, 0.0),
        (P_MIX, 0.30),
    ]),

    // --- Creative presets -------------------------------------------------

    // For a contact/piezo pickup (percussion, taps, scrapes). Fast transient
    // envelope, 20 bands for detail, lots of unvoiced noise to keep the
    // clicks/scrapes, a little grit. Tracks the source pitch.
    Preset::from_overrides("Piezo Perc", &[
        (P_ATTACK, 0.8),
        (P_RELEASE, 12.0),
        (P_BANDS, BANDS_20 as f32),
        (P_UNVOICED, 0.35),
        (P_DRIVE, 0.25),
        (P_PITCH_SOURCE, PITCH_VOICE as f32),
    ]),

    // Deep menacing robot. Saw+sub an octave down, formant lowered, moderate
    // grit — the sound of a movie villain AI.
    Preset::from_overrides("Deep Villain", &[
        (P_WAVE, WAVE_SAWSUB as f32),
        (P_PITCH, -12.0),
        (P_FORMANT, -5.0),
        (P_RELEASE, 45.0),
        (P_DRIVE, 0.30),
    ]),

    // Harsh sci-fi antagonist. Square carrier, 11 tinny bands, choppy fast
    // envelope, formant up, heavy drive. Angry and aggressive.
    Preset::from_overrides("Dalek Scream", &[
        (P_WAVE, WAVE_SQUARE as f32),
        (P_BANDS, BANDS_11 as f32),
        (P_ATTACK, 1.0),
        (P_RELEASE, 10.0),
        (P_FORMANT, 4.0),
        (P_DRIVE, 0.60),
    ]),

    // Lush, wide robotic choir. Saw+sub, 20 bands, slow release, big detune
    // for width, formant slightly up, airy.
    Preset::from_overrides("Angel Choir", &[
        (P_WAVE, WAVE_SAWSUB as f32),
        (P_BANDS, BANDS_20 as f32),
        (P_RELEASE, 90.0),
        (P_DETUNE, 14.0),
        (P_FORMANT, 2.0),
        (P_UNVOICED, 0.20),
        (P_MIX, 1.0),
    ]),

    // Otherworldly throat / chipmunk. Extreme upward formant shift, tight
    // envelope — a strange, shifted timbre.
    Preset::from_overrides("Alien Formant", &[
        (P_FORMANT, 9.0),
        (P_ATTACK, 2.0),
        (P_RELEASE, 25.0),
    ]),

    // Lo-fi console robot. Pulse carrier, 11 bands, no drive, no detune —
    // a dry chiptune vocoder.
    Preset::from_overrides("8-Bit Vox", &[
        (P_WAVE, WAVE_PULSE as f32),
        (P_BANDS, BANDS_11 as f32),
        (P_UNVOICED, 0.10),
    ]),

    // Warm vintage (Roland SVC-350 / EMS vibe). Saw, tanh warmth, formant a
    // touch down, medium release, a little detune.
    Preset::from_overrides("Analog 70s", &[
        (P_DRIVE, 0.30),
        (P_FORMANT, -1.0),
        (P_RELEASE, 35.0),
        (P_DETUNE, 6.0),
    ]),

    // Ambient texture over a pad. Sidechain carrier, slow smeared envelope,
    // wide detune, mostly wet — layer it on an external synth via the Carrier
    // input.
    Preset::from_overrides("Sci-Fi Texture", &[
        (P_SOURCE, SRC_SIDECHAIN as f32),
        (P_ATTACK, 15.0),
        (P_RELEASE, 120.0),
        (P_DETUNE, 15.0),
        (P_MIX, 0.90),
    ]),

    // Kubyz (jaw-harp) as modulator → talking sub bass. The SawSub carrier
    // tracks the kubyz's steady low drone; 20 bands catch its mouth-formant
    // sweeps. Voice pitch source (YIN-friendly stable drone). Pitch -12 drops
    // the carrier an octave so it reads as a real sub — the tracker otherwise
    // latches the kubyz's stronger low harmonic (~110-160 Hz), not its weak
    // 73 Hz fundamental.
    Preset::from_overrides("Kubyz Bass", &[
        (P_WAVE, WAVE_SAWSUB as f32),
        (P_BANDS, BANDS_20 as f32),
        (P_ATTACK, 5.0),
        (P_RELEASE, 55.0),
        (P_PITCH, -12.0),
        (P_DETUNE, 6.0),
        (P_FORMANT, -2.0),
        (P_UNVOICED, 0.20),
        (P_DRIVE, 0.35),
        (P_PITCH_SOURCE, PITCH_VOICE as f32),
        (P_MIX, 1.0),
    ]),

    // Voice → kubyz morph. Voice on the main input, a kubyz (sample or live)
    // into the Carrier sidechain input → your voice plays through the kubyz
    // timbre. The long release is the trick: when the voice stops, the band
    // envelopes fall slowly so the kubyz carrier rings out with the last
    // vowel's formant — the phrase "dissolves into" a voice-shaped kubyz drone.
    // Full wet; do the morph with a Dry/Wet automation in the DAW.
    Preset::from_overrides("Voice→Kubyz", &[
        (P_SOURCE, SRC_SIDECHAIN as f32),
        (P_ATTACK, 6.0),
        (P_RELEASE, 110.0),
        (P_UNVOICED, 0.12),
        (P_DRIVE, 0.0),
        (P_FORMANT, 0.0),
        (P_MIX, 1.0),
        (P_BANDS, BANDS_20 as f32),
    ]),

    // Evolving ambient pad from a kubyz. Slow attack + long release smear the
    // jaw-harp's mouth-wah into a drifting drone; wide detune adds movement.
    // For beds / Flow. Pitch source stays Auto → tracks the drone (no MIDI).
    Preset::from_overrides("Kubyz Drone", &[
        (P_WAVE, WAVE_SAWSUB as f32),
        (P_ATTACK, 40.0),
        (P_RELEASE, 150.0),
        (P_DETUNE, 14.0),
        (P_BANDS, BANDS_20 as f32),
        (P_FORMANT, 0.0),
        (P_UNVOICED, 0.10),
        (P_DRIVE, 0.0),
        (P_MIX, 1.0),
    ]),

    // Melodic lead: play the tune on a MIDI keyboard, the kubyz on the main
    // input supplies live mouth-formant "wah" articulation. Bright saw that
    // talks. Auto = keys when played, else tracks the kubyz.
    Preset::from_overrides("Kubyz Lead", &[
        (P_PITCH_SOURCE, PITCH_AUTO as f32),
        (P_WAVE, WAVE_SAW as f32),
        (P_ATTACK, 4.0),
        (P_RELEASE, 30.0),
        (P_DETUNE, 8.0),
        (P_BANDS, BANDS_20 as f32),
        (P_DRIVE, 0.25),
        (P_UNVOICED, 0.15),
        (P_MIX, 1.0),
    ]),

    // Aggressive Reese-style bass growl driven by kubyz. Wide detune gives the
    // Reese beating, heavy drive the grit. For drops / DnB.
    Preset::from_overrides("Kubyz Growl", &[
        (P_WAVE, WAVE_SAWSUB as f32),
        (P_PITCH, -12.0),
        (P_DRIVE, 0.60),
        (P_DETUNE, 18.0),
        (P_BANDS, BANDS_16 as f32),
        (P_ATTACK, 3.0),
        (P_RELEASE, 40.0),
        (P_FORMANT, -2.0),
        (P_UNVOICED, 0.15),
        (P_MIX, 1.0),
    ]),
];

pub fn apply(shared: &crate::SharedParamsInner, preset: &Preset) {
    use std::sync::atomic::Ordering;
    for (i, v) in preset.values.iter().enumerate() {
        if let Some(atom) = shared.params.get(i) {
            atom.store(*v, Ordering::Relaxed);
            // Mark dirty so the audio thread emits ParamValueEvents — a preset
            // switch (incl. Bands / Pitch Src) is recorded into the host's
            // automation lane instead of silently reverting on playback (#24).
            if let Some(d) = shared.dirty_params.get(i) {
                d.store(true, Ordering::Relaxed);
            }
        }
    }
}
