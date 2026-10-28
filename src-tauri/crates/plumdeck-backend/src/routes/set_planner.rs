//! Whole-set planning for MCP agents (app/services/set_planner_service.py,
//! domain/services/phased_setlist_builder.py, infra/rekordbox_playlists.py;
//! S9): rekordbox playlists read-only, phase pools, and the phased builder.

use std::collections::{BTreeMap, BTreeSet};

use plumdeck_core::compat::text::casefold;
use plumdeck_db::duckdb::Connection;
use plumdeck_db::repos::recommendations::{candidate_pool, PoolQuery, PoolRow};
use plumdeck_db::repos::wordplay::{approved_edges, PairRow};
use plumdeck_domain::assist::normalize_text;
use plumdeck_domain::mixability::{bpm_distance, cosine_similarity, mixability_score, SET_TRANSITION_WEIGHTS};
use plumdeck_domain::targets::{sanitize, RawTargets, Targets};
use plumdeck_rekordbox::masterdb::{PlaylistKind, PlaylistNode};
use plumdeck_rekordbox::MasterDb;
use regex::Regex;
use serde_json::{json, Value};

use crate::routes::setlists::{edge_map, wordplay_payload};

const MAX_PHASES: usize = 12;
const MAX_PHASE_COUNT: i64 = 60;
const MAX_SIMILAR_SEEDS: usize = 12;
const LIBRARY_POOL_LIMIT: i64 = 300;
const ARTIST_MATCH_LIMIT: i64 = 200;
const ENERGY_WEIGHT: f64 = 0.6;
const TEMPO_WEIGHT: f64 = 1.5;
const ANCHOR_BONUS: f64 = 0.3;
const PHASE_SWITCH_MIX_WEIGHT: f64 = 1.5;
const SAME_ARTIST_PENALTY: [f64; 2] = [0.35, 0.15];
const APPROVED_WORDPLAY_BONUS: f64 = 0.25;

pub type PlanResult = Result<Value, String>;

fn unavailable() -> String {
    "rekordbox のプレイリストを読み取れませんでした".into()
}

fn open() -> Result<MasterDb, String> {
    let path = plumdeck_rekordbox::locate::default_master_db_path().filter(|p| p.is_file()).ok_or("rekordbox のライブラリ (master.db) が見つかりません")?;
    MasterDb::open(&path).map_err(|_| "rekordbox のライブラリを読み取れませんでした".to_owned())
}

fn kind(node: &PlaylistNode) -> &'static str {
    match node.kind {
        PlaylistKind::Playlist => "playlist",
        PlaylistKind::Folder => "folder",
        PlaylistKind::Smart => "smart",
        PlaylistKind::Unknown(_) => "unknown",
    }
}

fn walk<'a>(roots: &'a [PlaylistNode], out: &mut Vec<&'a PlaylistNode>) {
    for node in roots {
        out.push(node);
        walk(&node.children, out);
    }
}

fn name_key(name: &str) -> String {
    let cleaned: String = name.chars().filter(|c| !c.is_control()).collect();
    normalize_text(Some(cleaned.trim()))
}

/// `find_node`: by ID, else by a unique `Folder/Sub/Name` path.
fn find_node<'a>(roots: &'a [PlaylistNode], reference: &str) -> Result<&'a PlaylistNode, String> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Err("playlist の ID またはパスを指定してください".into());
    }
    let mut all = Vec::new();
    walk(roots, &mut all);
    if let Some(node) = all.iter().find(|n| n.id == reference) {
        return Ok(node);
    }
    let wanted: Vec<String> = reference.split('/').filter(|p| !p.trim().is_empty()).map(name_key).collect();
    let matches: Vec<&&PlaylistNode> = all.iter().filter(|n| n.path.split('/').map(name_key).collect::<Vec<_>>() == wanted).collect();
    match matches.len() {
        0 => Err(format!("rekordbox にプレイリスト「{reference}」が見つかりません")),
        1 => Ok(matches[0]),
        _ => Err(format!("「{reference}」に一致するプレイリストが複数あります。ID で指定してください: {}", matches.iter().map(|n| n.id.as_str()).collect::<Vec<_>>().join(", "))),
    }
}

struct Entry {
    playlist_path: String,
    track_no: i64,
    filepath: String,
    title: String,
    artist: String,
}

fn playlist_entries(db: &MasterDb, node: &PlaylistNode, recursive: bool) -> Result<(Vec<Entry>, Vec<String>), String> {
    let mut notes = Vec::new();
    let mut playlists: Vec<&PlaylistNode> = Vec::new();
    fn collect<'a>(current: &'a PlaylistNode, root: &PlaylistNode, recursive: bool, playlists: &mut Vec<&'a PlaylistNode>, notes: &mut Vec<String>) {
        match current.kind {
            PlaylistKind::Playlist => playlists.push(current),
            PlaylistKind::Smart => notes.push(format!("「{}」はインテリジェントプレイリストのため読み込めません", current.path)),
            PlaylistKind::Folder if recursive || std::ptr::eq(current, root) => {
                for child in &current.children {
                    if matches!(child.kind, PlaylistKind::Folder) && !recursive {
                        continue;
                    }
                    collect(child, root, recursive, playlists, notes);
                }
            }
            _ => {}
        }
    }
    collect(node, node, recursive, &mut playlists, &mut notes);
    let mut entries = Vec::new();
    for playlist in playlists {
        for song in db.playlist_songs(&playlist.id).map_err(|_| unavailable())? {
            entries.push(Entry { playlist_path: playlist.path.clone(), track_no: song.track_no, filepath: song.folder_path, title: song.title, artist: song.artist });
        }
    }
    Ok((entries, notes))
}

/// `list_rekordbox_playlists`.
pub fn list_playlists(under: Option<&str>, depth: i64) -> PlanResult {
    let db = open()?;
    let roots = db.playlist_tree().map_err(|_| unavailable())?;
    let (nodes, base) = match under.filter(|u| !u.is_empty()) {
        Some(under) => {
            let node = find_node(&roots, under)?;
            (node.children.clone(), json!(node.path))
        }
        None => (roots.clone(), Value::Null),
    };
    fn payload(node: &PlaylistNode, level: i64, depth: i64) -> Value {
        let mut item = json!({"id": node.id, "name": node.name, "path": node.path, "kind": kind(node)});
        if matches!(node.kind, PlaylistKind::Folder) {
            item["child_count"] = json!(node.children.len());
            if level < depth {
                item["children"] = Value::Array(node.children.iter().map(|c| payload(c, level + 1, depth)).collect());
            }
        } else {
            item["track_count"] = json!(node.track_count);
        }
        item
    }
    Ok(json!({"under": base, "playlists": nodes.iter().map(|n| payload(n, 1, depth)).collect::<Vec<_>>()}))
}

fn track_ids_by_path(conn: &Connection, paths: &[String]) -> Result<BTreeMap<String, i64>, String> {
    let rows = plumdeck_db::repos::csv_io::all_tracks(conn).map_err(|e| e.to_string())?;
    let mut out = BTreeMap::new();
    for path in paths {
        if out.contains_key(path) {
            continue;
        }
        let found: Vec<&plumdeck_db::repos::tracks::TrackRow> = rows.iter().filter(|t| plumdeck_rekordbox::paths::same_path(path, &t.filepath)).collect();
        if found.len() == 1 {
            out.insert(path.clone(), found[0].id);
        }
    }
    Ok(out)
}

type Resolved = (String, String, Vec<(Entry, Option<i64>)>, Vec<String>);

fn resolve_playlist(conn: &Connection, reference: &str, recursive: bool) -> Result<Resolved, String> {
    let db = open()?;
    let roots = db.playlist_tree().map_err(|_| unavailable())?;
    let node = find_node(&roots, reference)?;
    let (entries, notes) = playlist_entries(&db, node, recursive)?;
    let paths: Vec<String> = entries.iter().filter(|e| !e.filepath.is_empty()).map(|e| e.filepath.clone()).collect();
    let ids = track_ids_by_path(conn, &paths)?;
    let rows = entries.into_iter().map(|e| {
        let id = ids.get(&e.filepath).copied();
        (e, id)
    });
    Ok((node.id.clone(), node.path.clone(), rows.collect(), notes))
}

/// `get_rekordbox_playlist_tracks`.
pub fn playlist_tracks(conn: &Connection, reference: &str, recursive: bool) -> PlanResult {
    let db = open()?;
    let roots = db.playlist_tree().map_err(|_| unavailable())?;
    let node_kind = kind(find_node(&roots, reference)?);
    let (id, path, rows, mut notes) = resolve_playlist(conn, reference, recursive)?;
    let ids: Vec<i64> = rows.iter().filter_map(|(_, id)| *id).collect();
    let tracks = plumdeck_db::repos::tracks::tracks_by_ids(conn, &ids).map_err(|e| e.to_string())?;
    let (mut out, mut unmatched) = (Vec::new(), Vec::new());
    for (entry, id) in &rows {
        let Some(track) = id.and_then(|id| tracks.iter().find(|t| t.id == id)) else {
            unmatched.push(json!({"playlist": entry.playlist_path, "title": entry.title, "artist": entry.artist, "filepath": entry.filepath}));
            continue;
        };
        out.push(json!({
            "id": track.id, "title": track.title, "artist": track.artist, "bpm": track.bpm, "key": track.key, "genre": track.genre,
            "subgenre": track.subgenre, "year": track.year, "energy": track.energy, "danceability": track.danceability,
            "duration": track.duration, "playlist": entry.playlist_path, "track_no": entry.track_no,
        }));
    }
    if !unmatched.is_empty() {
        notes.push(format!("{} 曲は plumdeck のライブラリに未登録のため候補になりません", unmatched.len()));
    }
    let count = out.len();
    let unmatched_count = unmatched.len();
    unmatched.truncate(30);
    Ok(json!({
        "playlist": {"id": id, "path": path, "kind": node_kind}, "count": count, "tracks": out,
        "unmatched_count": unmatched_count, "unmatched": unmatched, "notes": notes,
    }))
}

// ------------------------------------------------------------------ phases

#[derive(Clone)]
struct PoolNode {
    row: PoolRow,
    sources: Vec<Value>,
    only_from: Option<BTreeSet<i64>>,
}

struct Phase {
    label: String,
    count: i64,
    pool: Vec<PoolNode>,
    anchors: Vec<i64>,
    energy: Option<(f64, f64)>,
    bpm: Option<(f64, f64)>,
    keep_order: bool,
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64()
}

fn bounds(start: &Value, end: &Value) -> Option<(f64, f64)> {
    let (s, e) = (number(start), number(end));
    if s.is_none() && e.is_none() {
        return None;
    }
    let s2 = s.or(e)?;
    let e2 = e.or(Some(s2))?;
    Some((s2, e2))
}

fn clean_list(value: &Value) -> Vec<Value> {
    value.as_array().map(|items| items.iter().filter(|v| !v.is_null() && v.as_str() != Some("")).cloned().collect()).unwrap_or_default()
}

fn ints(value: &Value) -> Result<Vec<i64>, String> {
    clean_list(value).iter().map(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).ok_or_else(|| "invalid track id".to_owned())).collect()
}

fn fold(value: Option<&str>) -> String {
    let nfkc = plumdeck_core::compat::text::nfkc(value.unwrap_or(""));
    casefold(&nfkc).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn artists(row: &PoolRow) -> BTreeSet<String> {
    static SPLIT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let split = SPLIT.get_or_init(|| Regex::new(r"\s*(?:,|&|/|;|\bx\b|\bvs\.?|\bfeat\.?|\bft\.?|\bfeaturing\b|\bwith\b|\band\b)\s*").expect("regex"));
    split.split(&fold(row.artist.as_deref())).filter(|n| !n.is_empty()).map(str::to_owned).collect()
}

fn core_title(row: &PoolRow) -> String {
    static NOISE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let noise = NOISE.get_or_init(|| Regex::new(r"\s*[\(\[【].*?[\)\]】]|\s+-\s+.*$|\s+vs\.?\s+.*$").expect("regex"));
    let folded = fold(row.title.as_deref());
    let core = fold(Some(&noise.replace_all(&folded, "")));
    if core.is_empty() {
        folded
    } else {
        core
    }
}

fn same_song(left: &PoolRow, right: &PoolRow) -> bool {
    let title = core_title(left);
    !title.is_empty() && title == core_title(right) && !artists(left).is_disjoint(&artists(right))
}

fn transition_score(previous: &PoolRow, candidate: &PoolRow) -> f64 {
    let similarity = cosine_similarity(previous.vector.as_deref(), candidate.vector.as_deref(), previous.embedding_model.as_deref(), candidate.embedding_model.as_deref());
    mixability_score(previous.bpm, candidate.bpm, similarity, SET_TRANSITION_WEIGHTS)
}

fn key_relation(left: Option<&str>, right: Option<&str>) -> &'static str {
    let (Some(left), Some(right)) = (plumdeck_key::normalize_key(left), plumdeck_key::normalize_key(right)) else { return "unknown" };
    if left == right {
        return "same";
    }
    let parse = |code: &str| -> Option<(i32, char)> { Some((code[..code.len() - 1].parse().ok()?, code.chars().last()?)) };
    let (Some((ln, ll)), Some((rn, rl))) = (parse(left), parse(right)) else { return "clash" };
    let adjacent = (ln == rn && ll != rl) || (ll == rl && ((ln % 12) + 1 == rn || (rn % 12) + 1 == ln));
    if adjacent {
        "compatible"
    } else {
        "clash"
    }
}

fn source_bonus(kind: &str) -> f64 {
    match kind {
        "track" | "playlist" | "artist" => 0.12,
        _ => 0.0,
    }
}

fn ramp(bounds: Option<(f64, f64)>, progress: f64) -> Option<f64> {
    bounds.map(|(s, e)| s + (e - s) * progress)
}

#[allow(clippy::too_many_arguments)]
fn score(node: &PoolNode, previous: Option<&PoolRow>, history: &[PoolRow], phase: &Phase, progress: f64, slot: i64, edges: &BTreeSet<(i64, i64)>, bonus: f64) -> f64 {
    let mut total = 0.0;
    if let Some(previous) = previous {
        let mix = transition_score(previous, &node.row);
        total += mix * if slot == 0 { PHASE_SWITCH_MIX_WEIGHT } else { 1.0 };
        if edges.contains(&(previous.id, node.row.id)) {
            total += bonus;
        }
        let names = artists(&node.row);
        if !names.is_empty() {
            for (back, penalty) in SAME_ARTIST_PENALTY.iter().enumerate() {
                if history.len() > back && !names.is_disjoint(&artists(&history[history.len() - 1 - back])) {
                    total -= penalty;
                }
            }
        }
    }
    if let Some(target) = ramp(phase.energy, progress) {
        total -= ENERGY_WEIGHT * node.row.energy.map_or(0.5, |e| (e - target).abs());
    }
    if let Some(target) = ramp(phase.bpm, progress) {
        let distance = bpm_distance(Some(target), node.row.bpm);
        total -= TEMPO_WEIGHT * if distance.is_finite() { distance } else { 0.3 };
    }
    if phase.anchors.contains(&node.row.id) {
        total += ANCHOR_BONUS;
    }
    total += node.sources.iter().map(|s| source_bonus(s["type"].as_str().unwrap_or(""))).fold(0.0, f64::max);
    total
}

fn round(x: f64, digits: u32) -> f64 {
    plumdeck_core::compat::pyfmt::round_ndigits(x, digits)
}

fn describe(previous: Option<&PoolRow>, node: &PoolRow, edges: &BTreeSet<(i64, i64)>) -> Value {
    let Some(previous) = previous else { return Value::Null };
    let bpm_diff = match (previous.bpm.filter(|b| *b != 0.0), node.bpm.filter(|b| *b != 0.0)) {
        (Some(a), Some(b)) => json!(round(b - a, 1)),
        _ => Value::Null,
    };
    json!({
        "from_track_id": previous.id, "bpm_diff": bpm_diff, "key_relation": key_relation(previous.key.as_deref(), node.key.as_deref()),
        "mix_score": round(transition_score(previous, node), 3), "approved_wordplay": edges.contains(&(previous.id, node.id)),
    })
}

struct Placement {
    node: PoolNode,
    phase: String,
    phase_switch: bool,
    transition: Value,
}

fn build(phases: &[Phase], edges: &BTreeSet<(i64, i64)>, opening: Option<PoolRow>, bonus: f64) -> (Vec<Placement>, Vec<String>) {
    let mut placements = Vec::new();
    let mut notes = Vec::new();
    let mut used: BTreeSet<i64> = BTreeSet::new();
    let mut history: Vec<PoolRow> = Vec::new();
    if let Some(opening) = opening {
        used.insert(opening.id);
        history.push(opening);
    }
    for phase in phases {
        let mut by_id: Vec<PoolNode> = Vec::new();
        for node in &phase.pool {
            if !by_id.iter().any(|n| n.row.id == node.row.id) {
                by_id.push(node.clone());
            }
        }
        let mut anchor_ids: Vec<i64> = Vec::new();
        for id in &phase.anchors {
            if !anchor_ids.contains(id) && by_id.iter().any(|n| n.row.id == *id) && !used.contains(id) {
                anchor_ids.push(*id);
            }
        }
        let count = phase.count.max(anchor_ids.len() as i64);
        let mut placed = 0;
        for slot in 0..count {
            let progress = if count > 1 { slot as f64 / (count - 1) as f64 } else { 0.0 };
            let previous = history.last().cloned();
            let eligible = |node: &PoolNode| {
                !used.contains(&node.row.id)
                    && !history.iter().any(|h| same_song(&node.row, h))
                    && node.only_from.as_ref().is_none_or(|from| from.is_empty() || previous.as_ref().is_some_and(|p| from.contains(&p.id)))
            };
            let remaining: Vec<&PoolNode> = anchor_ids.iter().filter(|id| !used.contains(id)).filter_map(|id| by_id.iter().find(|n| n.row.id == *id)).collect();
            let choices: Vec<PoolNode> = if phase.keep_order {
                phase.pool.iter().filter(|n| eligible(n)).take(1).cloned().collect()
            } else if !remaining.is_empty() && remaining.len() as i64 >= count - slot {
                let filtered: Vec<PoolNode> = remaining.iter().filter(|n| eligible(n)).map(|n| (*n).clone()).collect();
                if filtered.is_empty() {
                    remaining.iter().map(|n| (*n).clone()).collect()
                } else {
                    filtered
                }
            } else {
                by_id.iter().filter(|n| eligible(n)).cloned().collect()
            };
            if choices.is_empty() {
                break;
            }
            let mut best: Option<(f64, PoolNode)> = None;
            for node in choices {
                let s = score(&node, previous.as_ref(), &history, phase, progress, slot, edges, bonus);
                let better = match &best {
                    None => true,
                    Some((bs, bn)) => s > *bs || (s == *bs && node.row.id < bn.row.id),
                };
                if better {
                    best = Some((s, node));
                }
            }
            let (_, best) = best.expect("non-empty");
            placements.push(Placement {
                transition: describe(previous.as_ref(), &best.row, edges),
                phase: phase.label.clone(),
                phase_switch: slot == 0 && !history.is_empty(),
                node: best.clone(),
            });
            used.insert(best.row.id);
            history.push(best.row);
            placed += 1;
        }
        if placed < phase.count {
            notes.push(format!("「{}」は候補が足りず {placed}/{} 曲しか配置できませんでした", phase.label, phase.count));
        }
        let missing = anchor_ids.iter().filter(|id| !used.contains(id)).count();
        if missing > 0 {
            notes.push(format!("「{}」の必須曲 {missing} 曲を配置できませんでした", phase.label));
        }
    }
    (placements, notes)
}

/// The plan's inputs besides the phases.
pub struct PlanOptions {
    pub exclude_track_ids: Vec<i64>,
    pub exclude_artists: Vec<String>,
    pub opening_track_id: Option<i64>,
    pub include_wordplay_targets: bool,
    pub wordplay_bonus: f64,
    pub save_as_setlist_name: Option<String>,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self { exclude_track_ids: Vec::new(), exclude_artists: Vec::new(), opening_track_id: None, include_wordplay_targets: true, wordplay_bonus: APPROVED_WORDPLAY_BONUS, save_as_setlist_name: None }
    }
}

fn excluded_artist(row: &PoolRow, names: &[String]) -> bool {
    let artist = casefold(row.artist.as_deref().unwrap_or("")).split_whitespace().collect::<Vec<_>>().join(" ");
    names.iter().any(|n| !n.is_empty() && artist.contains(n.as_str()))
}

/// `plan_phased_setlist`.
pub fn plan(conn: &Connection, phases: &[Value], options: PlanOptions, similar: &dyn Fn(i64, i64) -> Option<Vec<i64>>, now: chrono::NaiveDateTime) -> PlanResult {
    if !(0.0..=1.0).contains(&options.wordplay_bonus) {
        return Err("wordplay_bonus は 0〜1 で指定してください".into());
    }
    if phases.is_empty() {
        return Err("phases を1つ以上指定してください".into());
    }
    if phases.len() > MAX_PHASES {
        return Err(format!("phases は最大 {MAX_PHASES} 個までです"));
    }
    let db_error = |e: plumdeck_db::DbError| e.to_string();
    let excluded: BTreeSet<i64> = options.exclude_track_ids.iter().copied().collect();
    let excluded_artists: Vec<String> = options.exclude_artists.iter().map(|n| casefold(n).split_whitespace().collect::<Vec<_>>().join(" ")).collect();
    let wordplay: BTreeMap<(i64, i64), PairRow> = edge_map(approved_edges(conn, None).map_err(db_error)?);
    let mut notes: Vec<String> = Vec::new();
    let mut built = Vec::new();
    let mut summaries = Vec::new();
    for (index, spec) in phases.iter().enumerate() {
        let label = spec["label"].as_str().filter(|l| !l.is_empty()).map(str::to_owned).unwrap_or_else(|| format!("Phase {}", index + 1));
        let count = spec["count"].as_i64().filter(|c| (1..=MAX_PHASE_COUNT).contains(c) && spec["count"].is_i64());
        let Some(count) = count else { return Err(format!("「{label}」の count は 1〜{MAX_PHASE_COUNT} の整数で指定してください")) };
        let targets = sanitize(&RawTargets { year_min: number(&spec["year_min"]), year_max: number(&spec["year_max"]), ..RawTargets::default() });
        let strings = |v: &Value| -> Option<Vec<String>> {
            let list: Vec<String> = clean_list(v).iter().filter_map(|x| x.as_str().map(str::to_owned)).collect();
            (!list.is_empty()).then_some(list)
        };
        let (genres, subgenres) = (strings(&spec["genres"]), strings(&spec["subgenres"]));
        let mut pool: Vec<PoolNode> = Vec::new();
        let fetch = |ids: &[i64], filtered: bool, explicit: bool| -> Result<Vec<PoolRow>, String> {
            let mut wanted: Vec<i64> = Vec::new();
            for id in ids {
                if !wanted.contains(id) && (explicit || !excluded.contains(id)) {
                    wanted.push(*id);
                }
            }
            if wanted.is_empty() {
                return Ok(Vec::new());
            }
            let rows = candidate_pool(
                conn,
                &PoolQuery {
                    targets: if filtered { targets.clone() } else { Targets::default() },
                    genres: if filtered { genres.as_deref() } else { None },
                    subgenres: if filtered { subgenres.as_deref() } else { None },
                    limit: wanted.len() as i64,
                    exclude_ids: &[],
                    candidate_ids: Some(&wanted),
                },
            )
            .map_err(db_error)?;
            let mut rows = rows;
            rows.sort_by_key(|r| wanted.iter().position(|id| *id == r.id).unwrap_or(usize::MAX));
            Ok(rows)
        };
        let add = |pool: &mut Vec<PoolNode>, rows: Vec<PoolRow>, source: Value, explicit: bool, only_from: Option<BTreeSet<i64>>| {
            for row in rows {
                if !explicit && (excluded.contains(&row.id) || excluded_artist(&row, &excluded_artists)) {
                    continue;
                }
                let at = match pool.iter().position(|n| n.row.id == row.id) {
                    Some(at) => at,
                    None => {
                        pool.push(PoolNode { row, sources: Vec::new(), only_from: only_from.clone() });
                        pool.len() - 1
                    }
                };
                if !pool[at].sources.contains(&source) {
                    pool[at].sources.push(source.clone());
                }
            }
        };
        let anchors = ints(&spec["anchor_track_ids"])?;
        add(&mut pool, fetch(&anchors, false, true)?, json!({"type": "anchor"}), true, None);
        add(&mut pool, fetch(&ints(&spec["track_ids"])?, false, true)?, json!({"type": "track"}), true, None);
        let mut playlist_summaries = Vec::new();
        for reference in clean_list(&spec["rekordbox_playlists"]) {
            let reference = reference.as_str().map(str::to_owned).unwrap_or_else(|| reference.to_string());
            let (_, path, rows, playlist_notes) = resolve_playlist(conn, &reference, true)?;
            notes.extend(playlist_notes);
            let matched: Vec<i64> = rows.iter().filter_map(|(_, id)| *id).collect();
            for row in fetch(&matched, false, false)? {
                let detail = rows.iter().find(|(_, id)| *id == Some(row.id)).map(|(e, _)| e.playlist_path.clone()).unwrap_or_else(|| path.clone());
                add(&mut pool, vec![row], json!({"type": "playlist", "detail": detail}), false, None);
            }
            let unmatched = rows.iter().filter(|(_, id)| id.is_none()).count();
            playlist_summaries.push(json!({"path": path, "matched": matched.iter().collect::<BTreeSet<_>>().len(), "unmatched": unmatched}));
            if unmatched > 0 {
                notes.push(format!("「{path}」の {unmatched} 曲は plumdeck 未登録のため除外しました"));
            }
        }
        for artist in clean_list(&spec["artists"]) {
            let artist = artist.as_str().map(str::to_owned).unwrap_or_else(|| artist.to_string());
            let mut statement = conn.prepare("SELECT tracks.id FROM tracks WHERE tracks.artist ILIKE ? ORDER BY tracks.id LIMIT ?").map_err(|e| e.to_string())?;
            let ids: Vec<i64> = statement
                .query_map(plumdeck_db::duckdb::params![format!("%{artist}%"), ARTIST_MATCH_LIMIT], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            if ids.len() as i64 >= ARTIST_MATCH_LIMIT {
                notes.push(format!("「{label}」: アーティスト「{artist}」は {ARTIST_MATCH_LIMIT} 曲を超えるため先頭 {ARTIST_MATCH_LIMIT} 曲だけを候補にしました"));
            }
            let found = fetch(&ids, true, false)?;
            if found.is_empty() {
                notes.push(format!("「{label}」: アーティスト「{artist}」の曲が条件内に見つかりません"));
            }
            add(&mut pool, found, json!({"type": "artist", "detail": artist}), false, None);
        }
        let mut seeds = ints(&spec["similar_to_track_ids"])?;
        if spec["expand_similar"].as_bool().unwrap_or(false) {
            for node in &pool {
                if !seeds.contains(&node.row.id) {
                    seeds.push(node.row.id);
                }
            }
        }
        let per_seed = spec["similar_per_seed"].as_i64().filter(|n| *n != 0).unwrap_or(8).clamp(1, 30);
        let mut unique_seeds: Vec<i64> = Vec::new();
        for seed in seeds {
            if !unique_seeds.contains(&seed) {
                unique_seeds.push(seed);
            }
        }
        for seed in unique_seeds.into_iter().take(MAX_SIMILAR_SEEDS) {
            let Some(ids) = similar(seed, per_seed * 2) else { continue };
            let found: Vec<PoolRow> = fetch(&ids, true, false)?.into_iter().take(per_seed as usize).collect();
            add(&mut pool, found, json!({"type": "similar", "detail": seed}), false, None);
        }
        let energy = bounds(&spec["energy_start"], &spec["energy_end"]);
        let bpm = bounds(&spec["bpm_start"], &spec["bpm_end"]);
        if pool.is_empty() {
            let raw = RawTargets {
                year_min: number(&spec["year_min"]),
                year_max: number(&spec["year_max"]),
                energy: energy.map(|(a, b)| (a + b) / 2.0),
                bpm: bpm.map(|(a, b)| (a + b) / 2.0),
                ..RawTargets::default()
            };
            let excluded_list: Vec<i64> = excluded.iter().copied().collect();
            let rows = candidate_pool(
                conn,
                &PoolQuery { targets: sanitize(&raw), genres: genres.as_deref(), subgenres: subgenres.as_deref(), limit: LIBRARY_POOL_LIMIT, exclude_ids: &excluded_list, candidate_ids: None },
            )
            .map_err(db_error)?;
            add(&mut pool, rows, json!({"type": "filter"}), false, None);
        }
        if options.include_wordplay_targets && !wordplay.is_empty() {
            let sources: BTreeSet<i64> = pool.iter().map(|n| n.row.id).collect();
            let mut needed: BTreeMap<i64, BTreeSet<i64>> = BTreeMap::new();
            for (source, target) in wordplay.keys() {
                if sources.contains(source) && !sources.contains(target) {
                    needed.entry(*target).or_default().insert(*source);
                }
            }
            let ids: Vec<i64> = needed.keys().copied().collect();
            for row in fetch(&ids, true, false)? {
                let from = needed.get(&row.id).cloned().unwrap_or_default();
                let detail: Vec<i64> = from.iter().copied().collect();
                add(&mut pool, vec![row], json!({"type": "wordplay", "detail": detail}), false, Some(from));
            }
        }
        if pool.is_empty() {
            notes.push(format!("「{label}」の候補がありません"));
        }
        let mut source_counts: BTreeMap<String, i64> = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        for node in &pool {
            let kinds: BTreeSet<String> = node.sources.iter().filter_map(|s| s["type"].as_str().map(str::to_owned)).collect();
            for kind in kinds {
                if !source_counts.contains_key(&kind) {
                    order.push(kind.clone());
                }
                *source_counts.entry(kind).or_default() += 1;
            }
        }
        let counts: serde_json::Map<String, Value> = order.iter().map(|k| (k.clone(), json!(source_counts[k]))).collect();
        summaries.push(json!({"label": label, "requested": count, "pool_size": pool.len(), "pool_sources": counts, "playlists": playlist_summaries}));
        built.push(Phase {
            label,
            count,
            anchors: anchors.into_iter().filter(|id| pool.iter().any(|n| n.row.id == *id)).collect(),
            pool,
            energy,
            bpm,
            keep_order: spec["keep_order"].as_bool().unwrap_or(false),
        });
    }
    let opening = match options.opening_track_id {
        Some(id) => {
            let found = candidate_pool(conn, &PoolQuery { targets: Targets::default(), genres: None, subgenres: None, limit: 1, exclude_ids: &[], candidate_ids: Some(&[id]) }).map_err(db_error)?;
            Some(found.into_iter().next().ok_or_else(|| format!("opening_track_id {id} が見つかりません"))?)
        }
        None => None,
    };
    let edges: BTreeSet<(i64, i64)> = wordplay.keys().copied().collect();
    let (placements, build_notes) = build(&built, &edges, opening, options.wordplay_bonus);
    notes.extend(build_notes);
    let mut tracks = Vec::new();
    for (position, placement) in placements.iter().enumerate() {
        let row = &placement.node.row;
        let previous = placement.transition["from_track_id"].as_i64();
        let pair = previous.and_then(|p| wordplay.get(&(p, row.id)));
        let wordplay_json = match pair {
            Some(pair) => json!(wordplay_payload(pair).map_err(|_| "wordplay".to_owned())?),
            None => Value::Null,
        };
        tracks.push(json!({
            "position": position, "phase": placement.phase, "phase_switch": placement.phase_switch,
            "id": row.id, "title": row.title, "artist": row.artist, "bpm": row.bpm, "key": row.key, "genre": row.genre,
            "subgenre": row.subgenre, "year": row.year, "energy": row.energy, "duration": row.duration,
            "has_lyrics": row.has_lyrics, "file_exists": !row.filepath.is_empty() && std::path::Path::new(&row.filepath).exists(),
            "sources": placement.node.sources, "transition": placement.transition, "wordplay_json": wordplay_json,
        }));
    }
    for summary in &mut summaries {
        let label = summary["label"].clone();
        summary["placed"] = json!(tracks.iter().filter(|t| t["phase"] == label).count());
    }
    let missing = tracks.iter().filter(|t| t["file_exists"] == false).count();
    if missing > 0 {
        notes.push(format!("{missing} 曲は音源ファイルが見つかりません"));
    }
    let total: f64 = tracks.iter().map(|t| t["duration"].as_f64().unwrap_or(0.0)).sum();
    let track_data: Vec<Value> = tracks.iter().map(|t| json!({"id": t["id"], "wordplay_json": t["wordplay_json"]})).collect();
    let mut result = json!({
        "count": tracks.len(), "total_duration_sec": plumdeck_core::compat::pyfmt::round_to_int(total).unwrap_or(0),
        "phases": summaries, "tracks": tracks, "notes": notes, "track_data": track_data,
    });
    if let Some(name) = options.save_as_setlist_name.filter(|n| !n.is_empty()) {
        let created = plumdeck_db::repos::setlists::create_setlist(conn, &name, now).map_err(db_error)?;
        result["saved_setlist"] = json!({"id": created.id, "name": created.name});
    }
    Ok(result)
}
