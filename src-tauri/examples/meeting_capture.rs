//! Record a meeting's tracks for N seconds, to test capture without the app.
//!
//!     cargo run --example meeting_capture -- <seconds> [call|in_person] [out dir]
//!
//! Writes mic.wav (and system.wav on a call) and prints each track's length
//! and peak level. A system track peaking near -120 dBFS is silent: usually
//! the System Audio Recording permission is missing.

fn main() {
    use handy_app_lib::meetings::{MeetingMode, Recording};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let seconds: u64 = args.first().and_then(|s| s.parse().ok()).unwrap_or(10);
    let mode = match args.get(1).map(String::as_str) {
        Some("in_person") => MeetingMode::InPerson,
        _ => MeetingMode::Call,
    };
    let dir = args
        .get(2)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("handy-meeting-capture"));

    let started = std::time::Instant::now();
    let recording = match Recording::start(&dir, mode, None) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!("Started in {:?}", started.elapsed());
    println!(
        "Recording {seconds} s ({mode:?}) from {} into {}",
        recording.mic_label,
        dir.display()
    );
    if let Some(e) = &recording.system_error {
        println!("No system audio: {e}");
    }
    std::thread::sleep(std::time::Duration::from_secs(seconds));
    let elapsed = recording.elapsed();
    match recording.stop() {
        Ok(tracks) => {
            println!("Stopped after {:.2} s", elapsed.as_secs_f64());
            for t in tracks {
                println!(
                    "{}: {:.2} s, peak {:.1} dBFS, padded {:.2} s",
                    t.file, t.seconds, t.peak_dbfs, t.padded_seconds
                );
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
