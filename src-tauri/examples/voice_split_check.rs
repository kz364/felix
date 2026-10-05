//! Re-group a meeting's saved mic fingerprints (windows.bin) and show how
//! each voice compares with the user's print and the others. Scratch tool.
//!   cargo run --release --example voice_split_check -- <meeting dir> [mic name]
use handy_app_lib::meetings::{diarize, transcript::Source, voiceprint, windows};
use std::path::PathBuf;

fn unit(v: &[f32]) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    v.iter().map(|x| x / n).collect()
}
fn cos(a: &[f32], b: &[f32]) -> f32 {
    unit(a).iter().zip(unit(b)).map(|(x, y)| x * y).sum()
}

fn main() -> Result<(), String> {
    let dir = PathBuf::from(std::env::args().nth(1).ok_or("dir")?);
    let mic = std::env::args()
        .nth(2)
        .unwrap_or("MacBook Pro Microphone".into());
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let app = home.join("Library/Application Support/com.pais.handy");
    let print = voiceprint::load_or_nearest(&app, &mic);
    let tracks = windows::load(&dir).ok_or("no windows.bin")?;
    let (wins, embs) = tracks
        .into_iter()
        .find(|(s, _)| {
            *s == if std::env::var("SYS").is_ok() {
                Source::System
            } else {
                Source::Mic
            }
        })
        .ok_or("no mic")?
        .1;
    println!("{} windows, print {}", wins.len(), print.is_some());
    let stops: Vec<f32> = std::env::var("STOPS")
        .map(|s| s.split(',').map(|x| x.parse().unwrap()).collect())
        .unwrap_or(vec![0.0, -0.05, -0.1, -0.15]);
    for stop in stops {
        let g = diarize::Grouping {
            stop,
            ..Default::default()
        };
        let (mut labels, _) = diarize::group_voices_with(&embs, &g, &vec![None; embs.len()]);
        let before = labels.iter().max().map_or(0, |m| m + 1);
        diarize::merge_same_voices(&embs, &mut labels, &vec![None; embs.len()]);
        if std::env::var("SPLIT").is_ok() {
            diarize::split_mixed_voices(&embs, &mut labels, &vec![None; embs.len()]);
        }
        let n = labels.iter().max().map_or(0, |m| m + 1) as usize;
        println!("\nstop {stop}: {before} groups -> {n} after merge");
        let mut cents = vec![vec![0.0f32; embs[0].len()]; n];
        let mut secs = vec![0.0f32; n];
        let mut spans: Vec<Vec<(f32, f32)>> = vec![Vec::new(); n];
        for ((l, e), (a, b)) in labels.iter().zip(&embs).zip(&wins) {
            for (c, x) in cents[*l as usize].iter_mut().zip(unit(e)) {
                *c += x;
            }
            secs[*l as usize] += (b - a) as f32 * 0.03;
            spans[*l as usize].push((*a as f32 * 0.03, *b as f32 * 0.03));
        }
        for v in 0..n {
            let me = print.as_ref().map_or(f32::NAN, |p| cos(&cents[v], p));
            let others: Vec<String> = (0..n)
                .filter(|&o| o != v)
                .map(|o| format!("{o}:{:.2}", cos(&cents[v], &cents[o])))
                .collect();
            let first: Vec<String> = spans[v]
                .iter()
                .take(4)
                .map(|(a, b)| format!("{a:.0}-{b:.0}"))
                .collect();
            println!(
                "  voice {v}: {:>6.0}s {:>4} wins  me {me:.2}  [{}]  first {}",
                secs[v],
                spans[v].len(),
                others.join(" "),
                first.join(",")
            );
        }
        for v in 0..n as u32 {
            let idx: Vec<usize> = (0..labels.len()).filter(|&i| labels[i] == v).collect();
            if idx.len() < 20 {
                continue;
            }
            let sub: Vec<Vec<f32>> = idx.iter().map(|&i| embs[i].clone()).collect();
            let (mut sl, _) = diarize::group_voices_with(
                &sub,
                &diarize::Grouping::default(),
                &vec![None; sub.len()],
            );
            diarize::merge_same_voices(&sub, &mut sl, &vec![None; sub.len()]);
            let k = sl.iter().max().map_or(0, |m| m + 1) as usize;
            let mut c = vec![vec![0.0f32; embs[0].len()]; k];
            let mut t = vec![0.0f32; k];
            let mut first = vec![f32::MAX; k];
            for (j, &i) in idx.iter().enumerate() {
                let l = sl[j] as usize;
                for (a, x) in c[l].iter_mut().zip(unit(&embs[i])) {
                    *a += x;
                }
                t[l] += (wins[i].1 - wins[i].0) as f32 * 0.03;
                first[l] = first[l].min(wins[i].0 as f32 * 0.03);
            }
            let pair = if k == 2 { cos(&c[0], &c[1]) } else { f32::NAN };
            let me: Vec<String> = c
                .iter()
                .map(|x| {
                    print
                        .as_ref()
                        .map_or("-".into(), |p| format!("{:.2}", cos(x, p)))
                })
                .collect();
            println!(
                "  split voice {v}: {k} parts {:?}s first {:?} pair {pair:.2} me {:?}",
                t.iter().map(|x| x.round()).collect::<Vec<_>>(),
                first.iter().map(|x| x.round()).collect::<Vec<_>>(),
                me
            );
        }
        if let Ok(t) = std::env::var("AT") {
            for at in t.split(',') {
                let s: f32 = at.parse().unwrap();
                let hit: Vec<u32> = labels
                    .iter()
                    .zip(&wins)
                    .filter(|(_, (a, b))| {
                        (*a as f32 * 0.03) < s + 15.0 && (*b as f32 * 0.03) > s - 15.0
                    })
                    .map(|(l, _)| *l)
                    .collect();
                println!("  around {s}s: {hit:?}");
            }
        }
    }
    Ok(())
}
