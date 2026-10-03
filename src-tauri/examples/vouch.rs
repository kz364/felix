//! Vouch for meetings from before the remembered voices' cutoff and learn
//! from them, in the order given (run while Felix is quit, so it doesn't
//! write the store over). Each argument is `<meeting id>`, `<meeting
//! id>=1on1:<name>` (a 1-on-1 with that person) or `<meeting id>=trust`
//! (the call's named voices are right). Prints each remembered person's
//! clean speech afterwards.
//!
//! cargo run --example vouch -- 2026-10-01_08-32-29=1on1:Ron 2026-10-02_13-59-57 ...

use handy_app_lib::meetings::{manager, remembered};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn main() -> Result<(), String> {
    let app_data = PathBuf::from(std::env::var("HOME").map_err(|e| e.to_string())?)
        .join("Library/Application Support/com.pais.handy");
    let meetings = app_data.join("meetings");
    for arg in std::env::args().skip(1) {
        let (id, how) = arg.split_once('=').unwrap_or((&arg, ""));
        let mut store = remembered::load_store(&app_data);
        match how.split_once(':') {
            Some(("1on1", name)) => {
                store.one_on_one.insert(id.to_string(), name.to_string());
            }
            _ if how == "trust" => {
                if !store.trust_voices.iter().any(|m| m == id) {
                    store.trust_voices.push(id.to_string());
                }
            }
            _ => {
                if !store.learn_also.iter().any(|m| m == id) {
                    store.learn_also.push(id.to_string());
                }
            }
        }
        remembered::save_store(&app_data, &store)?;
        let dir = meetings.join(id);
        let info = manager::read_info(&dir).ok_or(format!("no meeting {id}"))?;
        let (named, _) = remembered::apply(&dir, &info.speakers, &BTreeMap::new());
        println!("{id}: voices recognised {named:?}");
    }
    let store = remembered::load_store(&app_data);
    for v in &store.voices {
        println!(
            "{}: {} windows (~{} s of clean speech) from {:?}",
            v.label(),
            v.windows,
            v.windows * 3 / 2,
            v.from.keys().collect::<Vec<_>>()
        );
    }
    Ok(())
}
