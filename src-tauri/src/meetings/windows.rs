//! Every fingerprint window of a meeting, kept with it (`windows.bin`), so
//! when the user says who spoke a paragraph the voices' prints can be made
//! again from the right windows (see [`super::remembered`]) without running
//! the voice model over the audio again. About 2 KB per 1.5 s of speech.
//!
//! Layout, little-endian: `FWIN1`, track count (u32); per track: source
//! (u8, 0 mic, 1 system), window count (u32), dimension (u32), the windows'
//! (from, to) frames (u32 pairs), then their fingerprints (f32).

use super::diarize::Prints;
use super::transcript::Source;
use std::path::Path;

pub const FILE: &str = "windows.bin";
const MAGIC: &[u8] = b"FWIN1";

pub fn save(dir: &Path, tracks: &[(Source, Prints)]) -> Result<(), String> {
    let mut out = MAGIC.to_vec();
    out.extend((tracks.len() as u32).to_le_bytes());
    for (source, (wins, embs)) in tracks {
        let dim = embs.first().map_or(0, Vec::len);
        out.push(match source {
            Source::Mic => 0,
            Source::System => 1,
        });
        out.extend((wins.len() as u32).to_le_bytes());
        out.extend((dim as u32).to_le_bytes());
        for &(from, to) in wins {
            out.extend((from as u32).to_le_bytes());
            out.extend((to as u32).to_le_bytes());
        }
        for e in embs {
            if e.len() != dim {
                return Err("Fingerprints of different sizes".into());
            }
            for x in e {
                out.extend(x.to_le_bytes());
            }
        }
    }
    let tmp = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, out)
        .and_then(|_| std::fs::rename(&tmp, dir.join(FILE)))
        .map_err(|e| format!("Couldn't save the voice windows: {e}"))
}

pub fn load(dir: &Path) -> Option<Vec<(Source, Prints)>> {
    let bytes = std::fs::read(dir.join(FILE)).ok()?;
    let mut r = Reader {
        bytes: bytes.strip_prefix(MAGIC)?,
    };
    let tracks = r.u32()?;
    let mut out = Vec::new();
    for _ in 0..tracks {
        let source = match r.take(1)?[0] {
            0 => Source::Mic,
            1 => Source::System,
            _ => return None,
        };
        let (n, dim) = (r.u32()? as usize, r.u32()? as usize);
        let wins = (0..n)
            .map(|_| Some((r.u32()? as usize, r.u32()? as usize)))
            .collect::<Option<Vec<_>>>()?;
        let embs = (0..n)
            .map(|_| (0..dim).map(|_| r.f32()).collect::<Option<Vec<_>>>())
            .collect::<Option<Vec<_>>>()?;
        out.push((source, (wins, embs)));
    }
    Some(out)
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        if self.bytes.len() < n {
            return None;
        }
        let (head, rest) = self.bytes.split_at(n);
        self.bytes = rest;
        Some(head)
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_come_back_as_saved() {
        let dir = std::env::temp_dir().join(format!("felix-windows-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tracks = vec![
            (
                Source::Mic,
                (
                    vec![(0, 50), (25, 75)],
                    vec![vec![0.6, 0.8], vec![1.0, 0.0]],
                ),
            ),
            (Source::System, (vec![], vec![])),
        ];
        save(&dir, &tracks).unwrap();
        assert_eq!(load(&dir), Some(tracks));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
