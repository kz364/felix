//! Work out a meeting's speaker names again, as Felix does after
//! transcription, and print them with how long each voice talked. Run it on
//! a copy (`<copy>/meetings/<id>`): it writes the meeting's evidence and
//! names, and the remembered voices next to `<copy>/meetings`.
//!
//!     cargo run --release --example meeting_names -- <copy>/meetings/<id> [--write]
//!
//! `--write` also saves the names into the meeting's `meeting.json`, to fix
//! a real meeting's names. Only with Felix quit, so it can't save over them.

use handy_app_lib::meetings::{pipeline, speakers};
use std::collections::BTreeMap;
use std::path::Path;

fn main() {
    let dir = std::env::args().nth(1).expect("meeting dir");
    let write = std::env::args().any(|a| a == "--write");
    let dir = Path::new(&dir);
    let names = speakers::apply(dir, std::env::var("ME").ok().as_deref());
    if write {
        let path = dir.join("meeting.json");
        let mut info: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("meeting.json"))
                .expect("meeting.json");
        info["app_speakers"] = serde_json::to_value(&names).expect("names");
        std::fs::write(&path, serde_json::to_vec_pretty(&info).expect("json")).expect("write");
        println!("saved the names into {}", path.display());
    }
    let t = pipeline::load(dir).expect("transcript");
    let mut talk: BTreeMap<u32, u64> = BTreeMap::new();
    for s in &t.segments {
        if let Some(v) = s.speaker {
            *talk.entry(v).or_default() += s.end_ms - s.start_ms;
        }
    }
    for (v, ms) in talk {
        let name = names.get(&v).map_or("(no name)", String::as_str);
        println!("voice {v}: {:>4} s  {name}", ms / 1000);
    }
}
