//! NVIDIA's Streaming Sortformer (diar_streaming_sortformer_4spk-v2.1, ONNX)
//! on AMI, scored like `speaker_eval ami` (phase 9 of the speaker spec:
//! evaluation only; not used by the app).
//!
//!   cargo run --release --example sortformer_eval -- <ami dir> [<model.onnx>]
//!
//! NeMo's offline operating point: 340-frame chunks (80 ms frames) with 40
//! frames of lookahead, a 40-frame FIFO and a 188-frame speaker cache kept
//! by NeMo's cache compression. Up to four speakers. ONLY=<ids> limits it.

use handy_app_lib::meetings::speaker_score::{from_rttm, from_uem, score, Score};
use handy_app_lib::meetings::transcript::FRAME_MS;
use ndarray::{s, Array1, Array2, Array3, Axis};
use ort::session::Session;
use ort::value::Value;
use rustfft::num_complex::Complex;
use std::path::{Path, PathBuf};
use std::time::Instant;

const SR: usize = 16_000;
const N_FFT: usize = 512;
const WIN: usize = 400;
const HOP: usize = 160;
const N_MELS: usize = 128;
const SUB: usize = 8;
const CHUNK: usize = 340;
const RIGHT: usize = 40;
const FIFO: usize = 40;
const CACHE: usize = 188;
const UPDATE: usize = 300;
const SPK: usize = 4;
const EMB: usize = 512;
const OUT_MS: f64 = 80.0;

fn slaney_mel(f: f64) -> f64 {
    if f < 1000.0 {
        3.0 * f / 200.0
    } else {
        15.0 + 27.0 * (f / 1000.0).ln() / 6.4f64.ln()
    }
}

fn slaney_hz(m: f64) -> f64 {
    if m < 15.0 {
        200.0 * m / 3.0
    } else {
        1000.0 * ((m - 15.0) * 6.4f64.ln() / 27.0).exp()
    }
}

/// librosa.filters.mel(sr=16000, n_fft=512, n_mels=128, fmax=8000), Slaney.
fn mel_filters() -> Vec<Vec<f32>> {
    let bins = N_FFT / 2 + 1;
    let (lo, hi) = (slaney_mel(0.0), slaney_mel(SR as f64 / 2.0));
    let hz: Vec<f64> = (0..N_MELS + 2)
        .map(|i| slaney_hz(lo + (hi - lo) * i as f64 / (N_MELS + 1) as f64))
        .collect();
    (0..N_MELS)
        .map(|m| {
            let norm = 2.0 / (hz[m + 2] - hz[m]);
            (0..bins)
                .map(|k| {
                    let f = k as f64 * SR as f64 / N_FFT as f64;
                    let up = (f - hz[m]) / (hz[m + 1] - hz[m]);
                    let down = (hz[m + 2] - f) / (hz[m + 2] - hz[m + 1]);
                    (up.min(down).max(0.0) * norm) as f32
                })
                .collect()
        })
        .collect()
}

/// NeMo's AudioToMelSpectrogramPreprocessor as Sortformer was trained:
/// pre-emphasis 0.97, centred 25 ms Hann frames every 10 ms, power
/// spectrum, 128 Slaney mels, log(x + 2^-24), no normalisation.
fn features(audio: &[f32]) -> Array2<f32> {
    let mut x = Vec::with_capacity(audio.len() + N_FFT);
    x.extend(std::iter::repeat_n(0.0f32, N_FFT / 2));
    let mut prev = 0.0;
    for &a in audio {
        x.push(a - 0.97 * prev);
        prev = a;
    }
    x.extend(std::iter::repeat_n(0.0f32, N_FFT / 2));
    let frames = 1 + audio.len() / HOP;
    let window: Vec<f32> = (0..WIN)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (WIN - 1) as f32).cos())
        .collect();
    let offset = (N_FFT - WIN) / 2;
    let filters = mel_filters();
    let fft = rustfft::FftPlanner::<f32>::new().plan_fft_forward(N_FFT);
    let mut out = Array2::<f32>::zeros((frames, N_MELS));
    let mut buf = vec![Complex::new(0.0, 0.0); N_FFT];
    for t in 0..frames {
        let start = t * HOP;
        for (i, b) in buf.iter_mut().enumerate() {
            let w = if i >= offset && i < offset + WIN {
                window[i - offset]
            } else {
                0.0
            };
            *b = Complex::new(x.get(start + i).copied().unwrap_or(0.0) * w, 0.0);
        }
        fft.process(&mut buf);
        let power: Vec<f32> = buf[..N_FFT / 2 + 1].iter().map(|c| c.norm_sqr()).collect();
        for (m, f) in filters.iter().enumerate() {
            let e: f32 = f.iter().zip(&power).map(|(a, b)| a * b).sum();
            out[[t, m]] = (e + 5.960_464_5e-8).ln();
        }
    }
    out
}

fn cat(a: &Array2<f32>, b: &Array2<f32>) -> Array2<f32> {
    ndarray::concatenate(Axis(0), &[a.view(), b.view()]).expect("same width")
}

struct Stream {
    cache: Array2<f32>,
    cache_preds: Array2<f32>,
    fifo: Array2<f32>,
    fifo_preds: Array2<f32>,
    sil_emb: Array1<f32>,
    sil_n: usize,
}

impl Stream {
    fn new() -> Self {
        Self {
            cache: Array2::zeros((0, EMB)),
            cache_preds: Array2::zeros((0, SPK)),
            fifo: Array2::zeros((0, EMB)),
            fifo_preds: Array2::zeros((0, SPK)),
            sil_emb: Array1::zeros(EMB),
            sil_n: 0,
        }
    }

    /// One chunk of features; returns its speaker probabilities (80 ms rows).
    fn step(&mut self, session: &mut Session, chunk: Array2<f32>) -> Result<Array2<f32>, String> {
        let len = chunk.nrows();
        let (cl, fl) = (self.cache.nrows(), self.fifo.nrows());
        let e = |e: ort::Error| e.to_string();
        let t3 = |a: Array2<f32>| Value::from_array(a.insert_axis(Axis(0))).map_err(e);
        let n = |v: usize| Value::from_array(Array1::from_vec(vec![v as i64])).map_err(e);
        let outputs = session
            .run(ort::inputs![
                "chunk" => t3(chunk)?,
                "chunk_lengths" => n(len)?,
                "spkcache" => t3(self.cache.clone())?,
                "spkcache_lengths" => n(cl)?,
                "fifo" => t3(self.fifo.clone())?,
                "fifo_lengths" => n(fl)?,
            ])
            .map_err(e)?;
        let (shape, data) = outputs["spkcache_fifo_chunk_preds"]
            .try_extract_tensor::<f32>()
            .map_err(e)?;
        let preds = Array3::from_shape_vec(
            (shape[0] as usize, shape[1] as usize, shape[2] as usize),
            data.to_vec(),
        )
        .map_err(|e| e.to_string())?
        .index_axis_move(Axis(0), 0);
        let (shape, data) = outputs["chunk_pre_encode_embs"]
            .try_extract_tensor::<f32>()
            .map_err(e)?;
        let embs = Array3::from_shape_vec(
            (shape[0] as usize, shape[1] as usize, shape[2] as usize),
            data.to_vec(),
        )
        .map_err(|e| e.to_string())?
        .index_axis_move(Axis(0), 0);

        let keep = CHUNK.min(len.div_ceil(SUB)).min(embs.nrows());
        let chunk_preds = preds.slice(s![cl + fl..cl + fl + keep, ..]).to_owned();
        self.cache_preds = preds.slice(s![..cl, ..]).to_owned();
        self.fifo_preds = cat(&preds.slice(s![cl..cl + fl, ..]).to_owned(), &chunk_preds);
        self.fifo = cat(&self.fifo, &embs.slice(s![..keep, ..]).to_owned());

        if self.fifo.nrows() > FIFO {
            let pop = UPDATE
                .max((keep + fl).saturating_sub(FIFO))
                .min(self.fifo.nrows());
            let pop_embs = self.fifo.slice(s![..pop, ..]).to_owned();
            let pop_preds = self.fifo_preds.slice(s![..pop, ..]).to_owned();
            for (row, p) in pop_embs.rows().into_iter().zip(pop_preds.rows()) {
                if p.iter().all(|&v| v < 0.2) {
                    self.sil_n += 1;
                    let k = 1.0 / self.sil_n as f32;
                    self.sil_emb = &self.sil_emb * (1.0 - k) + &(&row * k);
                }
            }
            self.fifo = self.fifo.slice(s![pop.., ..]).to_owned();
            self.fifo_preds = self.fifo_preds.slice(s![pop.., ..]).to_owned();
            let old = self.cache.nrows();
            self.cache = cat(&self.cache, &pop_embs);
            self.cache_preds = cat(&self.cache_preds, &pop_preds);
            if self.cache.nrows() > CACHE {
                let (embs, preds) = self.compress(&self.cache_preds, old);
                self.cache = embs;
                self.cache_preds = preds;
            }
        }
        Ok(chunk_preds)
    }

    /// NeMo's speaker-cache compression: keep, per speaker, the frames that
    /// most clearly hold that speaker alone, plus one silence slot each.
    fn compress(&self, preds: &Array2<f32>, latest_from: usize) -> (Array2<f32>, Array2<f32>) {
        let n = preds.nrows();
        let per_spk = CACHE / SPK - 1;
        let strong = (per_spk as f32 * 0.75) as usize;
        let weak = (per_spk as f32 * 1.5) as usize;
        let min_pos = (per_spk as f32 * 0.5) as usize;
        let th = 0.25f32;
        let mut scores = Array2::<f32>::zeros((n, SPK));
        for t in 0..n {
            let sum: f32 = (0..SPK).map(|s| (1.0 - preds[[t, s]]).max(th).ln()).sum();
            for s in 0..SPK {
                let p = preds[[t, s]];
                scores[[t, s]] = p.max(th).ln() - (1.0 - p).max(th).ln() + sum - 0.5f32.ln();
            }
        }
        let pos: Vec<usize> = (0..SPK)
            .map(|s| (0..n).filter(|&t| scores[[t, s]] > 0.0).count())
            .collect();
        for t in 0..n {
            for s in 0..SPK {
                if preds[[t, s]] <= 0.5 || (scores[[t, s]] <= 0.0 && pos[s] >= min_pos) {
                    scores[[t, s]] = f32::NEG_INFINITY;
                } else if t >= latest_from {
                    scores[[t, s]] += 0.05;
                }
            }
        }
        for (k, scale) in [(strong, 2.0f32), (weak, 1.0)] {
            for s in 0..SPK {
                let mut order: Vec<usize> = (0..n).collect();
                order.sort_by(|&a, &b| scores[[b, s]].total_cmp(&scores[[a, s]]));
                for &t in order.iter().take(k) {
                    if scores[[t, s]].is_finite() {
                        scores[[t, s]] -= scale * 0.5f32.ln();
                    }
                }
            }
        }
        // Flat index speaker-major over n + 1 rows (the last is silence).
        let rows = n + 1;
        let mut flat: Vec<(usize, f32)> = (0..SPK)
            .flat_map(|s| {
                let col: Vec<(usize, f32)> =
                    (0..n).map(|t| (s * rows + t, scores[[t, s]])).collect();
                col.into_iter()
                    .chain(std::iter::once((s * rows + n, f32::INFINITY)))
            })
            .collect();
        flat.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut chosen: Vec<(usize, bool)> = flat
            .iter()
            .take(CACHE)
            .map(|&(i, sc)| (i, sc == f32::NEG_INFINITY))
            .collect();
        chosen.sort_by_key(|&(i, _)| i);
        let mut embs = Array2::<f32>::zeros((CACHE, EMB));
        let mut out_preds = Array2::<f32>::zeros((CACHE, SPK));
        for (j, &(i, dead)) in chosen.iter().enumerate() {
            let t = i % rows;
            if dead || t >= n {
                embs.row_mut(j).assign(&self.sil_emb);
            } else {
                embs.row_mut(j).assign(&self.cache.row(t));
                out_preds.row_mut(j).assign(&preds.row(t));
            }
        }
        (embs, out_preds)
    }
}

fn diarize(session: &mut Session, audio: &[f32]) -> Result<Array2<f32>, String> {
    let feats = features(audio);
    let mut stream = Stream::new();
    let mut out = Array2::<f32>::zeros((0, SPK));
    let step = CHUNK * SUB;
    let mut start = 0;
    while start < feats.nrows() {
        let end = (start + step + RIGHT * SUB).min(feats.nrows());
        let p = stream.step(session, feats.slice(s![start..end, ..]).to_owned())?;
        out = cat(&out, &p);
        start += step;
    }
    Ok(out)
}

fn row(name: &str, s: &Score, secs: f64) {
    println!(
        "{name:<10} DER {:>5.1}%  missed {:>4.1}%  false alarm {:>4.1}%  confusion {:>4.1}%  people {:>2} → voices {:>2}  {secs:>5.0} s",
        100.0 * s.der(),
        100.0 * s.missed as f64 / s.reference.max(1) as f64,
        100.0 * s.false_alarm as f64 / s.reference.max(1) as f64,
        100.0 * s.confusion_rate(),
        s.reference_speakers,
        s.guessed_speakers,
    );
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().ok_or("sortformer_eval <ami dir> [model]")?);
    let model = args.next().map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").expect("HOME"))
            .join("Library/Application Support/com.pais.handy/models/diar_streaming_sortformer_4spk-v2.1.onnx")
    });
    let mut session = Session::builder()
        .map_err(|e| e.to_string())?
        .with_memory_pattern(false)
        .map_err(|e| e.to_string())?
        .with_intra_threads(4)
        .map_err(|e| e.to_string())?
        .commit_from_file(&model)
        .map_err(|e| e.to_string())?;
    let mut ids: Vec<String> = std::fs::read_dir(dir.join("rttm"))
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.path()
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
        })
        .collect();
    ids.sort();
    if let Ok(only) = std::env::var("ONLY") {
        ids.retain(|id| only.split(',').any(|o| o == id));
    }
    let (mut total, mut total_ref) = (Score::default(), Score::default());
    let started = Instant::now();
    for id in &ids {
        let wav = dir.join("audio").join(format!("{id}.Mix-Headset.wav"));
        let (Ok(rttm), Ok(uem)) = (
            std::fs::read_to_string(dir.join("rttm").join(format!("{id}.rttm"))),
            std::fs::read_to_string(dir.join("uem").join(format!("{id}.uem"))),
        ) else {
            continue;
        };
        if !wav.is_file() {
            continue;
        }
        let t = Instant::now();
        let audio = read(&wav)?;
        let preds = diarize(&mut session, &audio)?;
        let frames = audio.len() * 1000 / SR / FRAME_MS as usize;
        let reference = from_rttm(&rttm, FRAME_MS, frames);
        let scored = from_uem(&uem, FRAME_MS, frames);
        let best = |f: usize| -> (u32, f32) {
            let r = ((f as f64 * FRAME_MS as f64 + FRAME_MS as f64 / 2.0) / OUT_MS) as usize;
            let r = r.min(preds.nrows().saturating_sub(1));
            (0..SPK)
                .map(|s| (s as u32, preds.get([r, s]).copied().unwrap_or(0.0)))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap_or((0, 0.0))
        };
        let guess: Vec<Option<u32>> = (0..frames)
            .map(|f| {
                let (s, p) = best(f);
                (p > 0.5).then_some(s)
            })
            .collect();
        let guess_ref: Vec<Option<u32>> = (0..frames)
            .map(|f| (!reference[f].is_empty()).then_some(best(f).0))
            .collect();
        let sc = score(&reference, &guess, &scored);
        let sr = score(&reference, &guess_ref, &scored);
        row(id, &sc, t.elapsed().as_secs_f64());
        // For adding up runs of one meeting each (ONLY=<id>).
        for (k, x) in [("raw", &sc), ("rawref", &sr)] {
            println!(
                "{k} {} {} {} {} {} {}",
                x.reference,
                x.missed,
                x.false_alarm,
                x.confusion,
                x.reference_speakers,
                x.guessed_speakers
            );
        }
        total.add(&sc);
        total_ref.add(&sr);
    }
    row("all", &total, started.elapsed().as_secs_f64());
    row("all ref", &total_ref, 0.0);
    Ok(())
}

fn read(wav: &Path) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(wav).map_err(|e| e.to_string())?;
    if reader.spec().sample_rate as usize != SR || reader.spec().channels != 1 {
        return Err(format!("{}: need 16 kHz mono", wav.display()));
    }
    reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())
}
