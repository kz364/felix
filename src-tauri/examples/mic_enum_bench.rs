//! Cost of resolving preferred microphones: a cpal input-device scan versus
//! the cached path, which only rescans after a CoreAudio device-list change.
//!
//!     cargo run --release --example mic_enum_bench
use handy_app_lib::audio_toolkit::list_input_devices;
use std::time::Instant;

fn stats(mut ms: Vec<f64>) -> String {
    ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = ms.len();
    format!(
        "median {:.3} ms, p90 {:.3} ms, max {:.3} ms",
        ms[n / 2],
        ms[n * 9 / 10],
        ms[n - 1]
    )
}

fn main() {
    let t0 = Instant::now();
    let first = list_input_devices().expect("enumerate");
    println!(
        "scan, first call (cold): {:.2} ms, devices: {:?}",
        t0.elapsed().as_secs_f64() * 1000.0,
        first.iter().map(|d| d.name.as_str()).collect::<Vec<_>>()
    );
    let scans: Vec<f64> = (0..50)
        .map(|_| {
            let t = Instant::now();
            let _ = list_input_devices().expect("enumerate");
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    println!("scan, repeated (n=50):   {}", stats(scans));

    // Cached path as used at recording start: clone the cached list and pick
    // the first preferred device.
    let cache: Vec<(String, cpal::Device)> =
        first.into_iter().map(|d| (d.name, d.device)).collect();
    let preferred = [
        "Wireless Mic Rx".to_string(),
        "MacBook Pro Microphone".to_string(),
    ];
    let cached: Vec<f64> = (0..1000)
        .map(|_| {
            let t = Instant::now();
            let devices = cache.clone();
            let _chosen = preferred
                .iter()
                .find_map(|p| devices.iter().find(|(n, _)| n == p));
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    println!("cached lookup (n=1000):  {}", stats(cached));
}
