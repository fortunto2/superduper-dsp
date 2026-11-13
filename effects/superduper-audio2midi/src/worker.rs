//! The worker thread: drains the capture rings into takes, reacts to the Analyse trigger (the
//! parameter's rising edge, the GUI, the end of a take with Auto on) and runs the transcription
//! on a thread of its own so capture never stalls behind inference.

use crate::capture::{Recorder, Rx};
use crate::notes::{NoteSet, Record};
use crate::{export, Inner, Settings, P_ANALYSE, P_FRAME_THR, P_MAX_FRET, P_CAPO, P_MODE, P_ONSET_THR};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use superduper_dsp_sdk::slog;
use superduper_transcribe::Transcriber;

/// Analyses finished in this process: lets a test wait for the worker.
pub static ANALYSES_COMPLETED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn settings_from_params(p: impl Fn(usize) -> f32) -> Settings {
    Settings {
        mode: if p(P_MODE) >= 0.5 { superduper_transcribe::Mode::Guitar } else { superduper_transcribe::Mode::Any },
        capo: p(P_CAPO).round().clamp(0.0, 12.0) as u8,
        max_fret: p(P_MAX_FRET).round().clamp(3.0, 24.0) as u8,
        onset_thr: p(P_ONSET_THR),
        frame_thr: p(P_FRAME_THR),
    }
}

pub fn spawn(shared: &Arc<Inner>) {
    let weak = Arc::downgrade(shared);
    let _ = std::thread::Builder::new().name("sdsp-a2m-worker".into()).spawn(move || {
        let mut w = Worker::default();
        while let Some(inner) = weak.upgrade() {
            w.step(&inner);
            drop(inner);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
}

#[derive(Default)]
struct Worker {
    rx: Option<Rx>,
    rec: Recorder,
    handled_req: u32,
    handled_clear: u32,
}

impl Worker {
    fn step(&mut self, s: &Arc<Inner>) {
        // Order matters: read the request counters first, then drain, so a take closed just
        // before the request is in `rec` when the request is acted on.
        let req = s.analyse_req.load(Ordering::Acquire);
        let clear = s.clear_req.load(Ordering::Acquire);
        if let Some(fresh) = s.rx_slot.lock().take() {
            self.rx = Some(fresh);
            self.rec.reset_counters();
        }
        if let Some(rx) = self.rx.as_mut() {
            self.rec.drain(rx);
        }
        s.captured_secs.store(self.rec.total_seconds() as f32, Ordering::Relaxed);
        s.takes_n.store(self.rec.takes.len() as u32, Ordering::Relaxed);
        if self.rec.truncated {
            *s.status.lock() = format!("take longer than {} s: the rest is not kept", crate::capture::MAX_TAKE_SECONDS as u32);
        }

        if clear != self.handled_clear {
            self.handled_clear = clear;
            self.rec = Recorder::default();
            self.rec.reset_counters();
            s.publish(None, Arc::new(NoteSet::default()));
            *s.status.lock() = "cleared".into();
        }

        // Analyse parameter: a stored toggle that acts on its rising edge (the host sets it back to 0 to re-arm); a state load that restores 1 marks the edge as seen, so it never fires on load.
        let high = s.params[P_ANALYSE].load(Ordering::Relaxed) >= 0.5;
        let mut want = req != self.handled_req;
        self.handled_req = req;
        if high && !s.analyse_seen_high.load(Ordering::Relaxed) {
            want = true;
        }
        s.analyse_seen_high.store(high, Ordering::Relaxed);
        if want {
            self.start_analysis(s);
        }
    }

    fn start_analysis(&mut self, s: &Arc<Inner>) {
        if s.analysing.swap(true, Ordering::AcqRel) {
            s.rerun.store(true, Ordering::Relaxed);
            return;
        }
        let Some(take) = self.rec.best_take().cloned() else {
            s.analysing.store(false, Ordering::Release);
            *s.status.lock() = if s.record.lock().is_some() {
                "no audio captured in this session (notes are from the project); play the transport to capture".into()
            } else {
                "nothing captured yet: start the transport on a track with audio".into()
            };
            return;
        };
        self.rec.fresh_session_pending = true;
        // Keep the audio for a re-analysis with other settings: the next take replaces it.
        let settings = settings_from_params(|i| s.params[i].load(Ordering::Relaxed));
        let inner = Arc::clone(s);
        let spawned = std::thread::Builder::new().name("sdsp-a2m-analyse".into()).spawn(move || {
            struct Done<'a>(&'a Inner);
            impl Drop for Done<'_> {
                fn drop(&mut self) {
                    self.0.analysing.store(false, Ordering::Release);
                }
            }
            let _done = Done(&inner);
            run_analysis(&inner, &take, settings);
        });
        if spawned.is_err() {
            s.analysing.store(false, Ordering::Release);
        }
    }
}

/// Transcribe one take and publish the result. Blocking; also used directly by tests.
pub fn run_analysis(s: &Inner, take: &crate::capture::Take, settings: Settings) {
    s.progress.store(0.0, Ordering::Relaxed);
    *s.status.lock() = format!("analysing {:.1} s…", take.seconds());
    let t0 = std::time::Instant::now();
    let result = Transcriber::new().and_then(|t| {
        t.transcribe(&take.samples, take.sr, &settings, |f| {
            s.progress.store(f, Ordering::Relaxed);
            true
        })
    });
    let analysis = match result {
        Ok(Some(a)) => a,
        Ok(None) => return,
        Err(e) => {
            slog!("analysis failed: {e}");
            *s.status.lock() = format!("analysis failed: {e}");
            return;
        }
    };
    let mut notes = analysis.notes;
    for n in &mut notes {
        n.start += take.song_start;
        n.end += take.song_start;
    }
    let mut rec = Record {
        settings,
        take_start: take.song_start,
        duration: analysis.seconds,
        tempo: take.tempo,
        notes,
        midi_path: None,
        json_path: None,
    };
    export_record(s, &mut rec);
    let n = rec.notes.len();
    let set = Arc::new(NoteSet::from_notes(&rec.notes));
    let msg = match &rec.midi_path {
        Some(p) => format!("{n} notes in {:.1} s, saved {}", t0.elapsed().as_secs_f64(), p.display()),
        None => format!("{n} notes in {:.1} s (export failed)", t0.elapsed().as_secs_f64()),
    };
    s.publish(Some(Arc::new(rec)), set);
    *s.status.lock() = msg;
    ANALYSES_COMPLETED.fetch_add(1, Ordering::SeqCst);
    if s.rerun.swap(false, Ordering::Relaxed) {
        s.analyse_req.fetch_add(1, Ordering::Release);
    }
}

fn export_record(s: &Inner, rec: &mut Record) {
    let dir = s.out_dir.clone();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let track = s.scope.track.lock().0.clone();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mid = export::next_path(&dir, &track, now);
    if std::fs::write(&mid, superduper_transcribe::midi::write_smf(&rec.notes, rec.tempo)).is_ok() {
        rec.midi_path = Some(mid.clone());
    }
    let json = mid.with_extension("json");
    if std::fs::write(&json, serde_json::to_vec_pretty(&rec.to_json()).unwrap_or_default()).is_ok() {
        rec.json_path = Some(json);
    }
}
