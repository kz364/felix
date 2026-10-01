//! What "Transcribe again" does, for a meeting while Felix is quit: keep
//! the names given, clear the old transcript and mark it queued, so Felix
//! transcribes it again when it next starts.
//!
//! cargo run --example queue_retranscribe -- <meeting dir>...

use handy_app_lib::meetings::{clues, manager, speakers, summary, transcript};
use std::path::Path;

fn main() -> Result<(), String> {
    for dir in std::env::args().skip(1) {
        let dir = Path::new(&dir);
        let info = manager::read_info(dir).ok_or("no meeting.json")?;
        speakers::keep_names(dir, &info.speakers);
        for file in [
            transcript::FILE,
            summary::CLEANED_FILE,
            clues::FILE,
            clues::TURNS_FILE,
        ] {
            let _ = std::fs::remove_file(dir.join(file));
        }
        let path = dir.join("meeting.json");
        let mut json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        json["transcript"] = "queued".into();
        json["transcript_error"] = serde_json::Value::Null;
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&json).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        println!("{}: queued", dir.display());
    }
    Ok(())
}
