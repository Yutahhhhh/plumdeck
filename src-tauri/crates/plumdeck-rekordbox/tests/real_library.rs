//! Reads a real rekordbox library. Ignored by default; run with
//!
//! ```sh
//! PLUMDECK_RB_TEST_DB=/path/to/copy/of/master.db \
//! PLUMDECK_RB_XCHECK_OUT=/tmp/rb-xcheck.json \
//! cargo test -p plumdeck-rekordbox --test real_library -- --ignored --nocapture
//! ```
//!
//! - `PLUMDECK_RB_TEST_DB` (required): a master.db, ideally a copy. Opened
//!   read-only.
//! - `PLUMDECK_RB_TEST_SHARE`: the share folder holding `PIONEER/USBANLZ`
//!   when `contentFile.rb_local_path` is not usable (default: the library's
//!   own `SyncAnalysisDataRootPath`). Analysis files are only ever opened for
//!   reading.
//! - `PLUMDECK_RB_SAMPLE`: how many tracks to parse (default 300).
//! - `PLUMDECK_RB_XCHECK_OUT`: where to write a per-track JSON summary for an
//!   external cross-check (content IDs, counts and checksums only; no titles
//!   or paths).
//!
//! Only aggregates are printed.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use plumdeck_rekordbox::anlz::{ppth_matches, AnlzFile, WaveformKind};
use plumdeck_rekordbox::locate::{sibling, AnlzKind};
use plumdeck_rekordbox::masterdb::{Content, CueSlot, MasterDb};

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn opt_json<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "null".to_owned(), |v| v.to_string())
}

fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    let mid = values.len() / 2;
    Some(if values.len() % 2 == 1 { values[mid] } else { (values[mid - 1] + values[mid]) / 2.0 })
}

/// The DAT path of a track: `contentFile.rb_local_path` when it exists, else
/// the share root plus `AnalysisDataPath`.
fn dat_path(content: &Content, local: &HashMap<String, PathBuf>, share: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = local.get(&content.id).filter(|p| p.is_file()) {
        return Some(path.clone());
    }
    let raw = content.analysis_data_path.as_deref()?;
    let candidate = share?.join(raw.trim_start_matches('/'));
    candidate.is_file().then_some(candidate)
}

fn registered_contains_all(db: &MasterDb, probe: &[String]) -> bool {
    let registered = db.registered_paths().unwrap();
    probe.iter().all(|p| registered.contains(p))
}

#[test]
#[ignore = "needs a real rekordbox library (PLUMDECK_RB_TEST_DB)"]
fn real_library_reads_cleanly() {
    let db_path = PathBuf::from(std::env::var("PLUMDECK_RB_TEST_DB").expect("set PLUMDECK_RB_TEST_DB"));
    let sample_size: usize = std::env::var("PLUMDECK_RB_SAMPLE").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
    let started = std::time::Instant::now();
    let db = MasterDb::open(&db_path).expect("library opens");
    println!("open: {:?}, DBVersion known: {}", started.elapsed(), db.is_known_version().unwrap());

    // ------------------------------------------------------- table aggregates
    let t = std::time::Instant::now();
    let contents = db.contents().unwrap();
    println!("contents: {} live rows ({:?})", contents.len(), t.elapsed());
    let keys = contents.iter().filter(|c| c.key_name.is_some()).count();
    println!("  with key: {keys}, with BPM: {}", contents.iter().filter(|c| c.bpm().is_some()).count());
    let cues = db.all_cues().unwrap();
    let mut by_slot: BTreeMap<String, usize> = BTreeMap::new();
    for cue in &cues {
        let name = match cue.slot {
            CueSlot::Memory => "memory".to_owned(),
            CueSlot::Hot(s) if s < 8 => "hot A-H".to_owned(),
            CueSlot::Hot(_) => "hot I-P".to_owned(),
            CueSlot::Unknown(k) => format!("kind {k}"),
        };
        *by_slot.entry(name).or_default() += 1;
    }
    println!("cues: {} ({by_slot:?}), loops {} [{:?}]", cues.len(), cues.iter().filter(|c| c.is_loop()).count(), started.elapsed());
    let mut gains: Vec<f64> = db.mixer_params().unwrap().iter().filter_map(|m| m.gain_db()).collect();
    println!("mixer: {} rows with gain, median {:.2} dB", gains.len(), median(&mut gains).unwrap_or(f64::NAN));
    println!("my tags: {}, assignments: {} [{:?}]", db.my_tags().unwrap().len(), db.song_my_tags().unwrap().len(), started.elapsed());
    let tree = db.playlist_tree().unwrap();
    println!("playlists: {} rows, {} roots", db.playlists().unwrap().len(), tree.len());
    let sessions = db.play_sessions().unwrap();
    println!(
        "history: {} sessions, {} plays",
        sessions.len(),
        sessions.iter().map(|s| s.filepaths.len()).sum::<usize>()
    );
    println!("  [{:?}]", started.elapsed());
    println!("content files: {} [{:?}]", db.content_files().unwrap().len(), started.elapsed());
    let t = std::time::Instant::now();
    let songs: usize = db.playlists().unwrap().iter().map(|p| db.playlist_songs(&p.id).unwrap().len()).sum();
    println!("playlist songs: {songs} over all playlists ({:?})", t.elapsed());
    let t = std::time::Instant::now();
    let probe: Vec<String> = contents.iter().filter_map(|c| c.folder_path.clone()).step_by(97).take(256).collect();
    let found = db.contents_by_paths(&probe).unwrap();
    let single = probe.iter().take(20).filter(|p| db.content_at_path(p).map(|c| c.is_some()).unwrap_or(true)).count();
    println!("path lookups: {}/{} by batch, {single}/20 single ({:?})", found.len(), probe.len(), t.elapsed());
    assert!(registered_contains_all(&db, &probe));
    let registered = db.registered_paths().unwrap();
    assert_eq!(registered.len(), contents.iter().filter_map(|c| c.folder_path.as_ref()).collect::<std::collections::HashSet<_>>().len());

    // ------------------------------------------------------------ ANLZ sample
    let share = std::env::var("PLUMDECK_RB_TEST_SHARE").ok().map(PathBuf::from).or_else(|| db.analysis_data_root().unwrap());
    let local: HashMap<String, PathBuf> = db
        .content_files()
        .unwrap()
        .into_iter()
        .filter_map(|f| {
            let path = f.rb_local_path?;
            path.ends_with(".DAT").then(|| (f.content_id, PathBuf::from(path)))
        })
        .collect();
    let mut candidates: Vec<&Content> = contents.iter().filter(|c| c.analysis_data_path.is_some()).collect();
    candidates.sort_by(|a, b| a.id.cmp(&b.id));
    let step = (candidates.len() / sample_size.max(1)).max(1);
    let sample: Vec<&Content> = candidates.iter().step_by(step).take(sample_size).copied().collect();
    let cues_by_content: HashMap<&str, Vec<&plumdeck_rekordbox::masterdb::Cue>> =
        cues.iter().fold(HashMap::new(), |mut map, cue| {
            map.entry(cue.content_id.as_str()).or_default().push(cue);
            map
        });

    println!("sample ready [{:?}]", started.elapsed());
    let mut json = String::from("[\n");
    let (mut files, mut missing, mut errors, mut invalid, mut issues) = (0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut grids, mut phrases, mut path_match, mut path_total) = (0usize, 0usize, 0usize, 0usize);
    let mut moods: BTreeMap<String, usize> = BTreeMap::new();
    let mut slowest = std::time::Duration::ZERO;
    let (mut pwv6_1200, mut pwv6_total, mut pwv7_ok, mut pwv7_total) = (0usize, 0usize, 0usize, 0usize);
    let (mut end_over, mut pssi_on_downbeat, mut pssi_starts) = (0usize, 0usize, 0usize);
    let t = std::time::Instant::now();
    for (n, content) in sample.iter().enumerate() {
        let Some(dat) = dat_path(content, &local, share.as_deref()) else {
            missing += 1;
            continue;
        };
        let mut record = format!(
            "{{\"id\":{},\"bpm_x100\":{},\"key\":{},\"cues\":{{",
            json_str(&content.id),
            opt_json(content.bpm_x100),
            content.key_name.as_deref().map_or("null".to_owned(), json_str)
        );
        let own = cues_by_content.get(content.id.as_str()).map(Vec::as_slice).unwrap_or(&[]);
        let hot = own.iter().filter(|c| matches!(c.slot, CueSlot::Hot(_))).count();
        let memory = own.iter().filter(|c| c.slot == CueSlot::Memory).count();
        let loops = own.iter().filter(|c| c.is_loop()).count();
        let _ = write!(record, "\"hot\":{hot},\"memory\":{memory},\"loops\":{loops}}}");

        let mut beat_numbers: Option<Vec<u16>> = None;
        let mut structure = None;
        for kind in [AnlzKind::Dat, AnlzKind::Ext, AnlzKind::Ex2] {
            let path = sibling(&dat, kind);
            if !path.is_file() {
                continue;
            }
            let one = std::time::Instant::now();
            let parsed = AnlzFile::read(&path);
            slowest = slowest.max(one.elapsed());
            files += 1;
            let file = match parsed {
                Ok(file) => file,
                Err(error) => {
                    errors += 1;
                    eprintln!("parse error #{n} {}: {error}", kind.extension());
                    continue;
                }
            };
            invalid += file.invalid_tags().count();
            issues += file.issues.len();
            for (fourcc, reason) in file.invalid_tags() {
                eprintln!("invalid tag #{n} {}: {fourcc} {reason}", kind.extension());
            }
            let ext = kind.extension();
            if let Some(p) = file.path() {
                path_total += 1;
                if content.folder_path.as_deref().is_some_and(|audio| ppth_matches(p, audio)) {
                    path_match += 1;
                }
            }
            if let Some(grid) = file.beat_grid() {
                grids += 1;
                beat_numbers = Some(grid.beats.iter().map(|b| b.beat_number).collect());
                let raw: Vec<u8> = grid
                    .beats
                    .iter()
                    .flat_map(|b| [b.beat_number.to_be_bytes(), b.tempo_x100.to_be_bytes()].concat().into_iter().chain(b.time_ms.to_be_bytes()))
                    .collect();
                let _ = write!(record, ",\"{ext}_pqtz\":[{},{}]", grid.beats.len(), fnv(&raw));
            }
            if let Some(s) = file.song_structure() {
                phrases += 1;
                structure = Some(s.clone());
                *moods.entry(format!("{:?}", s.mood)).or_default() += 1;
                let list: Vec<String> = s
                    .phrases
                    .iter()
                    .map(|p| format!("[{},{},{},{},{},{},{},{}]", p.index, p.beat, p.kind, p.k1, p.k2, p.k3, p.fill, p.beat_fill))
                    .collect();
                let _ = write!(
                    record,
                    ",\"{ext}_pssi\":{{\"mood\":{},\"end\":{},\"bank\":{},\"masked\":{},\"phrases\":[{}]}}",
                    s.mood.raw(),
                    s.end_beat,
                    s.raw_bank,
                    s.masked,
                    list.join(",")
                );
            }
            for w in [
                WaveformKind::Preview,
                WaveformKind::TinyPreview,
                WaveformKind::Detail,
                WaveformKind::ColorPreview,
                WaveformKind::ColorDetail,
                WaveformKind::ThreeBandPreview,
                WaveformKind::ThreeBandDetail,
            ] {
                if let Some(wave) = file.waveform(w) {
                    if w == WaveformKind::ThreeBandPreview {
                        pwv6_total += 1;
                        pwv6_1200 += usize::from(wave.columns() == 1200);
                    }
                    if let (WaveformKind::ThreeBandDetail, Some(length)) = (w, content.length_s) {
                        pwv7_total += 1;
                        pwv7_ok += usize::from((wave.columns() as f64 / 150.0 - length as f64).abs() <= 2.0);
                    }
                    let _ = write!(record, ",\"{ext}_{}\":[{},{}]", w.fourcc(), wave.columns(), fnv(&wave.data));
                }
            }
            if let Some(c) = file.waveform_calibration() {
                let _ = write!(record, ",\"{ext}_PWVC\":[{},{},{},{}]", c.unknown, c.values[0], c.values[1], c.values[2]);
            }
            let pcob: usize = file.cue_lists().map(|l| l.entries.len()).sum();
            let pco2: usize = file.extended_cue_lists().map(|l| l.entries.len()).sum();
            let _ = write!(record, ",\"{ext}_cue_entries\":[{pcob},{pco2}]");
            if let Some(v) = file.vbr() {
                let _ = write!(record, ",\"{ext}_PVBR\":{}", v.entries.len());
            }
        }
        if let (Some(numbers), Some(s)) = (&beat_numbers, &structure) {
            end_over += usize::from(usize::from(s.end_beat) > numbers.len());
            for p in &s.phrases {
                pssi_starts += 1;
                let index = usize::from(p.beat).checked_sub(1);
                pssi_on_downbeat += usize::from(index.and_then(|i| numbers.get(i)) == Some(&1));
            }
        }
        record.push('}');
        if json.len() > 2 {
            json.push_str(",\n");
        }
        json.push_str(&record);
    }
    json.push_str("\n]\n");
    println!(
        "anlz: {} tracks sampled, {} without files, {files} files in {:?} (slowest {:?})",
        sample.len(),
        missing,
        t.elapsed(),
        slowest
    );
    println!("  errors {errors}, invalid tags {invalid}, envelope issues {issues}");
    println!("  PWV6 with 1200 columns {pwv6_1200}/{pwv6_total}, PWV7 within 2 s of Length {pwv7_ok}/{pwv7_total}");
    println!("  PSSI starts on a 1-based PQTZ downbeat {pssi_on_downbeat}/{pssi_starts}, end_beat past grid {end_over}");
    println!("  PQTZ {grids}, PSSI {phrases} {moods:?}, PPTH = FolderPath {path_match}/{path_total}");
    if let Ok(out) = std::env::var("PLUMDECK_RB_XCHECK_OUT") {
        std::fs::write(&out, json).unwrap();
        println!("wrote cross-check summary");
    }
    assert!(sample.len() >= 200.min(candidates.len()));
    assert_eq!(errors, 0);
    assert_eq!(invalid, 0);
    assert_eq!(issues, 0);
}
