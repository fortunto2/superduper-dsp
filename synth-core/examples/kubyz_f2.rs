//! Три серии щипков кубыза с разными F2 при неподвижных F1/F3.
//! Траектория рта не участвует: голос принимает форманты как числа,
//! так что «остановить движение, оставить резонатор» — это просто их не менять.
//!
//! cargo run --release --example kubyz_f2 -- out.wav
//!
//! ЧТО ЭТОТ ПРИМЕР ПОКАЗАЛ (2026-09-07, измерено на его же выходе):
//! разницы между тремя положениями F2 почти нет, и не потому что резонатор
//! слаб, а потому что его нечем возбуждать.
//!
//!   средний уровень вокруг F1 = 705 Гц    -28.5 dB
//!   средний уровень вокруг F2 = 1301 Гц   -65.0 dB
//!   средний уровень вокруг F3 = 2165 Гц   -68.8 dB
//!   95% энергии ниже 759 Гц, 99% ниже 897 Гц
//!
//! 16 гармоник при строе 68.93 Гц кончаются на 1103 Гц. Заводской набор
//! формант 705/1301/2165 ставит ДВА из трёх выше всего спектра инструмента;
//! набор, снятый с живой записи (809/1091/1242), — один. Рабочий диапазон
//! F2 на этом строе примерно 400–1000 Гц, а не 1301.
//!
//! Следствие для спора «аддитив против физмодели»: у настоящего варгана
//! свистящие мouth-резонансы живут заметно выше 1.1 кГц, так что 16 гармоник
//! это не приближение к инструменту, а его нижняя треть. Поднять до ~48
//! (3.3 кГц на этом строе) — и вопрос про гласный оттенок станет задаваемым.
//! Спектр трёх серий: kubyz_f2_spectrum.svg рядом.
//!
//! УТОЧНЕНИЕ (одно окно на щипок, Hann 8192, +20 мс от атаки, общая шкала):
//! «три кривые лежат друг на друге» верно для 1301 и 2100 и НЕВЕРНО для 900.
//!
//!   гармоника    Гц     F2=900  F2=1301  F2=2100
//!         H11   758.2    -10.2    -13.3    -13.2
//!         H13   896.1    -15.2    -25.8    -24.6
//!         H15  1034.0    -25.2    -33.9    -38.4
//!         H19  1309.7    -71.2    -54.4    -82.2
//!
//! F2=900 поднимает H12..H15 на 6–11 dB, попадая на H13 (896 Гц), где энергия
//! есть. F2=1301 свою область тоже поднимает — на 17–28 dB, — но с уровня
//! -82 до -54 dB: резонатор работает честно, материала нет. То есть F2 живёт
//! примерно до 1 кГц на этом строе, а не «не работает вовсе».
//!
//! Форма ряда — подпись варгана, а не спад: максимум на H6 (414 Гц), H1 на
//! 53 dB тише неё (излучатель мал относительно длины волны 68.93 Гц), и
//! только после горба идёт -12.6 dB/окт. Разбор с atlas-relay на доске.

use superduper_synth_core::kubyz::voice::{KubyzParams, KubyzVoice};
use superduper_synth_core::kubyz::N_HARMONICS;

const SR: f32 = 48_000.0;
const ROOT_HZ: f32 = 68.93; // C#2 −10 c, строй нашего язычка

fn harmonics_bashkir() -> [f32; N_HARMONICS] {
    let db: [f32; N_HARMONICS] = [
        0.0, 6.6, 19.0, 24.1, 17.0, 38.6, 9.4, 16.7, 15.2, 17.9, 19.9, 9.8, 14.3, -0.5, 7.6, 3.3,
    ];
    let mut out = [0.0_f32; N_HARMONICS];
    for i in 0..N_HARMONICS {
        out[i] = 10f32.powf(db[i] / 20.0);
    }
    let max = out.iter().copied().fold(0.0_f32, f32::max).max(1e-6);
    for s in out.iter_mut() {
        *s /= max;
    }
    out
}

/// Щипковая огибающая: атака 3 мс, спад 0.4 с, сустейна нет —
/// язычок дёргают один раз, он звенит и умирает.
fn pluck_env(t: f32) -> f32 {
    const A: f32 = 0.003;
    const D: f32 = 0.40;
    if t < A {
        t / A
    } else {
        (-(t - A) / D * 3.0).exp()
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out_path = args.get(1).cloned().unwrap_or_else(|| "kubyz_f2.wav".into());

    let harmonics = harmonics_bashkir();
    let f2_positions = [900.0_f32, 1301.0, 2100.0]; // ниже рабочей точки, в ней, выше
    let (f1, f3) = (705.0_f32, 2165.0_f32);
    let bw = [200.0_f32, 300.0, 400.0];
    let gain = [1.0_f32, 0.9, 0.75];

    let pluck_s = 0.9_f32;
    let plucks_per_series = 4;
    let gap_s = 0.6_f32;

    let mut samples: Vec<(f32, f32)> = Vec::new();
    for (i, &f2) in f2_positions.iter().enumerate() {
        for _ in 0..plucks_per_series {
            let mut v = KubyzVoice::default();
            v.on_note_on(SR);
            let n = (SR * pluck_s) as usize;
            for k in 0..n {
                let t = k as f32 / SR;
                let (l, r) = v.process(KubyzParams {
                    sr: SR,
                    root_hz: ROOT_HZ,
                    harmonics: &harmonics,
                    formant_f: [f1, f2, f3],
                    formant_bw: bw,
                    formant_gain: gain,
                    formant_mix: 1.0,
                    velocity_formant_shift: 0.0, // velocity одинакова во всех сериях
                });
                let e = pluck_env(t);
                samples.push((l * e, r * e));
            }
        }
        if i + 1 < f2_positions.len() {
            for _ in 0..((SR * gap_s) as usize) {
                samples.push((0.0, 0.0));
            }
        }
    }

    let peak = samples
        .iter()
        .map(|&(l, r)| l.abs().max(r.abs()))
        .fold(0.0_f32, f32::max)
        .max(1e-9);
    let norm = 0.7 / peak;

    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: SR as u32,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(&out_path, spec).expect("create wav");
    for (l, r) in samples {
        for s in [l * norm, r * norm] {
            let q = (s.clamp(-1.0, 1.0) * 8_388_607.0) as i32;
            w.write_sample(q).unwrap();
        }
    }
    w.finalize().unwrap();
    eprintln!(
        "wrote {out_path}: F2 = {:?} Hz при F1={f1} F3={f3}, {} щипков в серии",
        f2_positions, plucks_per_series
    );
}
