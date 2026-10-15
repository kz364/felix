//! A meeting made from audio recorded elsewhere: a phone's voice memo, or a
//! clip someone sent. It's transcribed and summarised like one Felix
//! recorded, as an in-person meeting on one track. Several files become one
//! meeting in the order they were recorded, each after the first marked as
//! resumed. Files are read by what's in them, not their name (WhatsApp can
//! save a voice recording as "….jpeg").

use super::capture::MeetingMode;
use super::manager::{MeetingInfo, MeetingStatus};
use super::track::{TrackSummary, SAMPLE_RATE};
use crate::audio_toolkit::audio::FrameResampler;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Seconds from 1904 (when MP4 times start) to 1970.
const MP4_EPOCH_OFFSET: i64 = 2_082_844_800;

struct Clip {
    path: PathBuf,
    /// When it was recorded (Unix ms), if the file says.
    recorded_at: Option<i64>,
    /// 16 kHz mono.
    samples: Vec<f32>,
}

impl Clip {
    fn ms(&self) -> i64 {
        (self.samples.len() as u64 * 1000 / SAMPLE_RATE as u64) as i64
    }
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The whole file as 16 kHz mono.
fn decode(path: &Path) -> Result<Vec<f32>, String> {
    use rodio::Source;
    let file = File::open(path).map_err(|e| format!("Couldn't open {}: {e}", name_of(path)))?;
    let decoder = rodio::Decoder::try_from(file)
        .map_err(|_| format!("{} isn't audio Felix can read", name_of(path)))?;
    let channels = decoder.channels().max(1) as usize;
    let rate = decoder.sample_rate() as usize;
    let interleaved: Vec<f32> = decoder.collect();
    let mono: Vec<f32> = interleaved
        .chunks(channels)
        .map(|c| c.iter().sum::<f32>() / c.len() as f32)
        .collect();
    let mut out = Vec::with_capacity(mono.len() * SAMPLE_RATE as usize / rate.max(1) + 1);
    let mut resampler = FrameResampler::new(rate, SAMPLE_RATE as usize, Duration::from_millis(30));
    resampler.push(&mono, |f| out.extend_from_slice(f));
    resampler.finish(|f| out.extend_from_slice(f));
    if out.is_empty() {
        return Err(format!("{} has no audio in it", name_of(path)));
    }
    Ok(out)
}

/// When an MP4/M4A file was recorded, from its movie header.
fn mp4_recorded_at(file: &mut (impl Read + Seek)) -> Option<i64> {
    let end = file.seek(SeekFrom::End(0)).ok()?;
    let moov = find_box(file, 0, end, b"moov")?;
    let mvhd = find_box(file, moov.0, moov.1, b"mvhd")?;
    file.seek(SeekFrom::Start(mvhd.0)).ok()?;
    let mut head = [0u8; 12];
    file.read_exact(&mut head).ok()?;
    let secs = match head[0] {
        0 => u32::from_be_bytes(head[4..8].try_into().ok()?) as i64,
        1 => i64::try_from(u64::from_be_bytes(head[4..12].try_into().ok()?)).ok()?,
        _ => return None,
    };
    (secs > MP4_EPOCH_OFFSET).then(|| (secs - MP4_EPOCH_OFFSET) * 1000)
}

/// The body (start, end) of the first `kind` box between `from` and `to`.
fn find_box(
    file: &mut (impl Read + Seek),
    from: u64,
    to: u64,
    kind: &[u8; 4],
) -> Option<(u64, u64)> {
    let mut at = from;
    while at + 8 <= to {
        file.seek(SeekFrom::Start(at)).ok()?;
        let mut head = [0u8; 8];
        file.read_exact(&mut head).ok()?;
        let mut size = u32::from_be_bytes(head[..4].try_into().ok()?) as u64;
        let mut body = at + 8;
        if size == 1 {
            let mut large = [0u8; 8];
            file.read_exact(&mut large).ok()?;
            size = u64::from_be_bytes(large);
            body += 8;
        } else if size == 0 {
            size = to - at;
        }
        if size < body - at || at + size > to {
            return None;
        }
        if &head[4..] == kind {
            return Some((body, at + size));
        }
        at += size;
    }
    None
}

/// When the file was recorded: its own header, or else when it was last
/// written less how long it is.
fn recorded_at(path: &Path, ms: i64) -> Option<i64> {
    let mut file = File::open(path).ok()?;
    if let Some(at) = mp4_recorded_at(&mut file) {
        return Some(at);
    }
    let modified = file.metadata().ok()?.modified().ok()?;
    let at = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis() as i64;
    Some(at - ms)
}

/// In recording order when every file says when it was recorded;
/// otherwise as they were picked.
fn in_order(clips: &mut [Clip]) {
    if clips.iter().all(|c| c.recorded_at.is_some()) {
        clips.sort_by_key(|c| c.recorded_at);
    }
}

fn peak_dbfs(samples: &[f32]) -> f32 {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    20.0 * peak.max(1e-6).log10()
}

/// Read the files and save them as a new meeting in `meetings_dir`, ready
/// to transcribe.
pub fn import(meetings_dir: &Path, paths: &[PathBuf]) -> Result<MeetingInfo, String> {
    if paths.is_empty() {
        return Err("No files were chosen".into());
    }
    let mut clips = Vec::new();
    for path in paths {
        let samples = decode(path)?;
        let mut clip = Clip {
            path: path.clone(),
            recorded_at: None,
            samples,
        };
        clip.recorded_at = recorded_at(path, clip.ms());
        clips.push(clip);
    }
    in_order(&mut clips);

    let started_at = clips[0]
        .recorded_at
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    let last = clips.last().expect("at least one clip");
    let total_ms: i64 = clips.iter().map(Clip::ms).sum();
    let ended_at = last
        .recorded_at
        .map(|at| at + last.ms())
        .filter(|&end| end >= started_at + total_ms)
        .unwrap_or(started_at + total_ms);

    let mut id_at = started_at;
    let (id, dir) = loop {
        let id = chrono::DateTime::from_timestamp_millis(id_at)
            .unwrap_or_default()
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d_%H-%M-%S")
            .to_string();
        let dir = meetings_dir.join(&id);
        if !dir.exists() {
            break (id, dir);
        }
        id_at += 1000;
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let wav = dir.join("mic.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut resumed_at_ms = Vec::new();
    let mut written = 0u64;
    let mut peak = f32::MIN;
    {
        let mut writer = hound::WavWriter::create(&wav, spec).map_err(|e| e.to_string())?;
        for (i, clip) in clips.iter().enumerate() {
            if i > 0 {
                resumed_at_ms.push(written * 1000 / SAMPLE_RATE as u64);
            }
            for &s in &clip.samples {
                let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                writer.write_sample(v).map_err(|e| e.to_string())?;
            }
            written += clip.samples.len() as u64;
            peak = peak.max(peak_dbfs(&clip.samples));
        }
        writer.finalize().map_err(|e| e.to_string())?;
    }

    let names: Vec<String> = clips.iter().map(|c| name_of(&c.path)).collect();
    let mut info = MeetingInfo {
        id,
        mode: MeetingMode::InPerson,
        status: MeetingStatus::Recorded,
        started_at,
        ended_at: Some(ended_at),
        mic: format!("Imported: {}", names.join(", ")),
        system_error: None,
        tracks: vec![TrackSummary {
            file: "mic.wav".into(),
            source: "mic".into(),
            seconds: written as f64 / SAMPLE_RATE as f64,
            peak_dbfs: peak,
            padded_seconds: 0.0,
        }],
        title: None,
        transcript: None,
        transcript_error: None,
        title_is_auto: false,
        summary: None,
        summary_error: None,
        speakers: BTreeMap::new(),
        output_device: None,
        call_app: None,
        resumed_at_ms,
        languages: vec![],
        app_speakers: BTreeMap::new(),
        mode_auto: false,
        event_id: None,
        notion_page_id: None,
        notion_url: None,
        notion_error: None,
        notion_shared: false,
        notion_shareable: false,
        slack_sent_at: None,
        slack_error: None,
        title_from_call: false,
    };
    super::manager::write_info(&dir, &info);
    // The calendar meeting it was recorded in names it, as when recording.
    if let Some(invite) = super::calendar::for_meeting(&dir, started_at, ended_at) {
        if !invite.title.trim().is_empty() {
            info.title = Some(invite.title);
            super::manager::write_info(&dir, &info);
        }
    }
    log::info!(
        "Meeting {} imported from {} file(s), {:.0} s",
        info.id,
        clips.len(),
        written as f64 / SAMPLE_RATE as f64
    );
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn mp4_box(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    #[test]
    fn an_m4a_says_when_it_was_recorded_even_with_its_header_last() {
        // 2026-10-06 06:35:51 UTC, as a voice memo stamps it.
        let secs = (1_791_268_551 + MP4_EPOCH_OFFSET) as u32;
        let mut mvhd = vec![0u8; 4];
        mvhd.extend_from_slice(&secs.to_be_bytes());
        mvhd.extend_from_slice(&[0u8; 8]);
        let mut file = mp4_box(b"ftyp", b"M4A \0\0\0\0");
        file.extend(mp4_box(b"mdat", &[7u8; 100]));
        file.extend(mp4_box(b"moov", &mp4_box(b"mvhd", &mvhd)));
        assert_eq!(
            mp4_recorded_at(&mut Cursor::new(file)),
            Some(1_791_268_551_000)
        );
        assert_eq!(mp4_recorded_at(&mut Cursor::new(vec![0xff; 64])), None);
    }

    #[test]
    fn clips_go_in_recording_order_only_when_all_say_when() {
        let clip = |name: &str, at: Option<i64>| Clip {
            path: name.into(),
            recorded_at: at,
            samples: vec![],
        };
        let mut clips = vec![clip("b", Some(20)), clip("a", Some(10))];
        in_order(&mut clips);
        assert_eq!(clips[0].path, PathBuf::from("a"));
        let mut clips = vec![clip("b", Some(20)), clip("a", None)];
        in_order(&mut clips);
        assert_eq!(clips[0].path, PathBuf::from("b"));
    }

    #[test]
    fn a_wav_named_like_a_photo_is_read_as_16_khz_mono() {
        let dir = std::env::temp_dir().join(format!("felix-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("WhatsApp Image.jpeg");
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..48_000 {
            let v = ((i as f32 / 48.0).sin() * 8000.0) as i16;
            w.write_sample(v).unwrap();
            w.write_sample(v).unwrap();
        }
        w.finalize().unwrap();
        let samples = decode(&path).unwrap();
        assert!(
            (samples.len() as i64 - 16_000).abs() < 400,
            "{}",
            samples.len()
        );

        let info = import(&dir, &[path]).unwrap();
        assert_eq!(info.mode, MeetingMode::InPerson);
        assert!((info.tracks[0].seconds - 1.0).abs() < 0.05);
        assert!(dir.join(&info.id).join("mic.wav").is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
