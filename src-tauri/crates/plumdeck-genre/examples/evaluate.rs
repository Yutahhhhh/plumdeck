//! Held-out evaluation on an exported library:
//! `cargo run --release -p plumdeck-genre --example evaluate -- tracks.jsonl [hidden] [epochs]`
//! where each line is `{"genre","subgenre","bpm","brightness","loudness",
//! "contrast","noisiness","embedding_json"}` of a verified track.

use plumdeck_genre::{train, AudioFeatures, LabeledTrack, TrainConfig};
use serde_json::Value;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let text = std::fs::read_to_string(&args[1]).expect("read input");
    let f = |v: &Value, k: &str| v[k].as_f64().map(|x| x as f32);
    let tracks: Vec<LabeledTrack> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let v: Value = serde_json::from_str(line).ok()?;
            let embedding: Vec<f32> = serde_json::from_str::<Vec<f32>>(v["embedding_json"].as_str()?).ok()?;
            Some(LabeledTrack {
                features: AudioFeatures { embedding, brightness: f(&v, "brightness"), loudness: f(&v, "loudness"), contrast: f(&v, "contrast"), noisiness: f(&v, "noisiness"), bpm: f(&v, "bpm") },
                genre: v["genre"].as_str()?.to_owned(),
                subgenre: v["subgenre"].as_str().unwrap_or("").to_owned(),
            })
        })
        .collect();
    let mut config = TrainConfig { refit: false, ..TrainConfig::default() };
    if let Some(h) = args.get(2) {
        config.hidden = h.parse().unwrap();
    }
    if let Some(e) = args.get(3) {
        config.epochs = e.parse().unwrap();
    }
    let started = std::time::Instant::now();
    let model = train(&tracks, &config, &|_| {}).expect("train");
    let m = &model.metrics;
    println!(
        "tracks={} genres={} pairs={} hidden={} epochs={} time={:.1}s\ngenre acc={:.3} top3={:.3} subgenre acc={:.3}\nhigh-conf coverage={:.2} acc={:.3} low share={:.2} ECE={:.3} T=({:.2},{:.2})",
        m.samples, model.genres.len(), model.pairs.len(), config.hidden, config.epochs, started.elapsed().as_secs_f32(),
        m.genre_accuracy, m.genre_top3_accuracy, m.subgenre_accuracy, m.high_confidence_coverage, m.high_confidence_accuracy, m.low_confidence_share,
        m.calibration_error, model.genre_temperature, model.pair_temperature
    );
    for g in &m.per_genre {
        println!("  {:<12} n={:<5} recall={:.2}", g.genre, g.support, g.recall);
    }
}
