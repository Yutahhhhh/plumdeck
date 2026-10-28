//! The MCP server (backend/mcp_server; S9): Streamable HTTP at `/mcp`
//! (JSON-RPC 2.0, answered as SSE when the client accepts it) and
//! `GET /api/mcp/info`.
//!
//! The tools and prompts are the contract in contracts/mcp/. Each tool runs
//! the same service code as the HTTP routes, mostly by calling the ported
//! route in-process and reshaping its answer the way the Python tool did.
//! Analysis tools run the job service of S11; Junction tools go through
//! [`crate::routes::junction`].

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Map, Value};
use tower::ServiceExt;

use crate::response::PyJson;
use crate::routes::assist;
use crate::AppState;

const TOOLS: &str = include_str!("../../../../../contracts/mcp/tools.v1.json");
const PROMPTS: &str = include_str!("../../../../../contracts/mcp/prompts.v1.json");
const INSTRUCTIONS: &str = include_str!("../../../../../contracts/mcp/instructions.txt");
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

fn tools() -> Vec<Value> {
    serde_json::from_str(TOOLS).unwrap_or_default()
}

fn port() -> String {
    std::env::var("PLUMDECK_PORT").ok().filter(|p| p.parse::<u16>().is_ok()).unwrap_or_else(|| "8001".into())
}

/// `GET /api/mcp/info`.
pub async fn info() -> Response {
    let mut list: Vec<(String, String)> = tools()
        .iter()
        .map(|t| (t["name"].as_str().unwrap_or_default().to_owned(), t["description"].as_str().unwrap_or_default().to_owned()))
        .collect();
    list.sort();
    let port = port();
    let value = json!({
        "server_name": "plumdeck",
        "port": port.parse::<u16>().unwrap_or(8001),
        "http_path": "/mcp",
        "url": format!("http://127.0.0.1:{port}/mcp"),
        "tool_count": list.len(),
        "tools": list.into_iter().map(|(name, description)| json!({"name": name, "description": description})).collect::<Vec<_>>(),
    });
    match plumdeck_core::compat::pyjson_loads::loads(&value.to_string()) {
        Ok(parsed) => PyJson(parsed).into_response(),
        Err(_) => crate::server_error(&"mcp info"),
    }
}

/// `GET /mcp`: no server-initiated stream.
pub async fn get_stream() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "POST, DELETE")], "Method Not Allowed").into_response()
}

/// `DELETE /mcp`: end a session (sessions hold no state here).
pub async fn delete_session() -> Response {
    StatusCode::OK.into_response()
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// `POST /mcp`.
pub async fn post(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let accept = headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
    let sse = accept.contains("text/event-stream");
    let message: Value = match serde_json::from_slice(&body) {
        Ok(message) => message,
        Err(_) => return reply(rpc_error(Value::Null, -32700, "Parse error"), sse, None),
    };
    let mut session = headers.get("mcp-session-id").and_then(|v| v.to_str().ok()).map(str::to_owned);
    let batch = message.is_array();
    let messages = if let Value::Array(items) = message { items } else { vec![message] };
    let mut replies = Vec::new();
    for message in messages {
        let id = message.get("id").cloned();
        let method = message.get("method").and_then(Value::as_str).unwrap_or("").to_owned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id else { continue };
        let result = match method.as_str() {
            "initialize" => {
                let requested = params["protocolVersion"].as_str().unwrap_or("");
                let version = if PROTOCOL_VERSIONS.contains(&requested) { requested } else { PROTOCOL_VERSIONS[0] };
                session.get_or_insert_with(|| uuid::Uuid::new_v4().simple().to_string());
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {
                        "experimental": {},
                        "prompts": {"listChanged": false},
                        "resources": {"subscribe": false, "listChanged": false},
                        "tools": {"listChanged": false},
                    },
                    "serverInfo": {"name": "plumdeck", "title": "plumdeck Music Library", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS.trim_end(),
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "prompts/list" => Ok(json!({"prompts": serde_json::from_str::<Value>(PROMPTS).unwrap_or(json!([]))})),
            "prompts/get" => prompt(&params),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("").to_owned();
                let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(call_tool(&state, &name, arguments).await)
            }
            _ => Err((-32601, "Method not found".to_owned())),
        };
        replies.push(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            // The SDK names the unknown method in `data`.
            Err((-32601, message)) => {
                let mut error = rpc_error(id, -32601, &message);
                error["error"]["data"] = json!(method);
                error
            }
            Err((code, message)) => rpc_error(id, code, &message),
        });
    }
    if replies.is_empty() {
        let mut response = StatusCode::ACCEPTED.into_response();
        if let Some(session) = session.and_then(|s| HeaderValue::from_str(&s).ok()) {
            response.headers_mut().insert("mcp-session-id", session);
        }
        return response;
    }
    let payload = if batch { Value::Array(replies) } else { replies.remove(0) };
    reply(payload, sse, session)
}

fn reply(payload: Value, sse: bool, session: Option<String>) -> Response {
    let text = serde_json::to_string(&payload).unwrap_or_default();
    let mut response = if sse {
        ([(header::CONTENT_TYPE, "text/event-stream"), (header::CACHE_CONTROL, "no-cache, no-transform")], format!("event: message\r\ndata: {text}\r\n\r\n"))
            .into_response()
    } else {
        ([(header::CONTENT_TYPE, "application/json")], text).into_response()
    };
    if let Some(session) = session.and_then(|s| HeaderValue::from_str(&s).ok()) {
        response.headers_mut().insert("mcp-session-id", session);
    }
    response
}

fn prompt(params: &Value) -> Result<Value, (i64, String)> {
    let name = params["name"].as_str().unwrap_or("");
    let argument = |key: &str| params["arguments"][key].as_str().unwrap_or("").to_owned();
    let text = match name {
        "setlist_from_rekordbox_playlists" => {
            let playlists = argument("playlists");
            let minutes = argument("minutes");
            format!(
                "rekordbox のプレイリストを素材に、展開のあるセットリストを plumdeck で組んでください。\n\n対象プレイリスト（DJ の指定）: {}\n想定時間: {}\n\n手順:\n1. list_rekordbox_playlists で階層を確認し、指定されたフォルダ/プレイリストを特定する。\n2. get_rekordbox_playlist_tracks で各プレイリストの曲・BPM・キー・ジャンル・エネルギーを確認する。\n3. プレイリストをどの順で展開させるか（例: 現場物/イベ2 で温めて 現場物/HIPHOP&Latin で切り替える）、\n   各展開の曲数、エネルギー/BPM の推移を DJ の意図に沿って決め、短く説明して確認を取る。\n   曲数は 想定時間 ÷ 平均の使用尺（DJ に聞くか 1 曲 3〜4 分）で見積もる。\n4. plan_phased_setlist に展開を phases として渡す。切り替えが急なら間に\n   similar_to_track_ids や genres を使ったブリッジ用の短いフェーズを挟む。\n5. 結果の transition（bpm_diff, key_relation, mix_score）と notes を見て、弱い繋ぎを\n   指摘し、必要なら条件を変えて再生成する。保存は DJ の了承後に\n   save_as_setlist_name か set_setlist_tracks(track_data=...) で行う。\n\n除外したいアーティストがいれば exclude_artists に渡す。",
                if playlists.is_empty() { "未指定 — list_rekordbox_playlists で候補を見せて選んでもらう".to_owned() } else { playlists },
                if minutes.is_empty() { "未指定 — DJ に確認する".to_owned() } else { minutes },
            )
        }
        "setlist_questionnaire" => {
            let theme = argument("theme");
            format!(
                "DJ に設問形式で好みを聞き取り、plumdeck のローカル音源だけでセットリストを組んでください。\n\nテーマ・メモ: {}\n\n進め方（1 回に 1〜3 問、選択肢を添えて短く聞く）:\n1. 現場と尺: どんな場所/時間帯か、何分のセットか、客層。\n2. 流れ: 例「じわじわ上げる」「序盤から飛ばして中盤で落とす」「ジャンルを切り替えて見せ場を作る」。\n   答えから 3〜5 個の展開（phase）と、それぞれのエネルギー/BPM の推移を決める。\n3. 好きなアーティスト・曲: search_tracks で実在を確認し、候補を番号付きで提示して選んでもらう。\n   展開ごとに誰を軸にしたいかも聞く。絶対かけたい曲は anchor_track_ids にする。\n4. 広げ方: 選んだアーティストだけか、似た曲も混ぜるか（similar_to_track_ids / expand_similar）、\n   年代・ジャンルの縛り、避けたいアーティスト。\n5. ワードプレイ: list_wordplay_pairs で承認済みペアを確認する。選んだ曲の歌詞から\n   find_wordplay_links で新しい繋ぎ候補を探し、良いものは propose_wordplay_pairs で pending 登録して\n   DJ に承認を求める（承認されたものだけが生成に効く。承認は DJ の明示が必要）。\n6. 聞き取った内容を phases にまとめて plan_phased_setlist を呼ぶ。各展開の label には\n   意図がわかる名前を付ける。好きなアーティストは artists、選んだ曲は track_ids/anchor_track_ids。\n7. 結果を「展開 / 曲 / 選ばれた理由(sources) / 繋ぎ」で提示し、気に入らない点を聞いて調整する。\n   保存は DJ の了承後に save_as_setlist_name か set_setlist_tracks(track_data=...) で行う。\n\nアーティストの類似性は音響ベクトルだけでは分からないので、似たアーティストはあなた自身の\n音楽知識で挙げ、search_tracks でライブラリにあるか確かめてから artists に加える。\n除外したいアーティストがいれば exclude_artists に渡す。",
                if theme.is_empty() { "なし".to_owned() } else { theme },
            )
        }
        _ => return Err((-32602, format!("Unknown prompt: {name}"))),
    };
    let description = serde_json::from_str::<Vec<Value>>(PROMPTS)
        .unwrap_or_default()
        .into_iter()
        .find(|p| p["name"] == name)
        .map(|p| p["description"].clone())
        .unwrap_or(Value::Null);
    Ok(json!({"description": description, "messages": [{"role": "user", "content": {"type": "text", "text": text}}]}))
}

// ------------------------------------------------------------------ tools

/// Why a tool failed: `Some` text is shown after the prefix (ToolError),
/// `None` hides the cause (any other exception in Python).
type ToolResult = Result<Value, Option<String>>;

async fn call_tool(state: &AppState, name: &str, arguments: Value) -> Value {
    let known = tools().iter().any(|t| t["name"] == name);
    let result = if known { run(state, name, &arguments).await } else { Err(Some(format!("Unknown tool: {name}"))) };
    match result {
        Ok(value) => {
            let text = serde_json::to_string_pretty(&value).unwrap_or_default();
            json!({"content": [{"type": "text", "text": text}], "structuredContent": {"result": value}, "isError": false})
        }
        Err(message) => {
            let text = match message {
                Some(message) if known => format!("Error executing tool {name}: {message}"),
                Some(message) => message,
                None => format!("Error executing tool {name}"),
            };
            json!({"content": [{"type": "text", "text": text}], "isError": true})
        }
    }
}

/// A ported route called in-process: (status, JSON body or text).
async fn api(state: &AppState, method: Method, uri: &str, body: Option<Value>) -> (u16, Value) {
    let app = crate::routes::api().with_state(state.clone());
    let mut request = Request::builder().method(method).uri(uri).header(header::HOST, "127.0.0.1");
    let body = match body {
        Some(value) => {
            request = request.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::to_vec(&value).unwrap_or_default())
        }
        None => Body::empty(),
    };
    let Ok(request) = request.body(body) else { return (500, Value::Null) };
    let Ok(response) = app.oneshot(request).await;
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap_or_default();
    let value = serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, value)
}

async fn ok(state: &AppState, method: Method, uri: &str, body: Option<Value>) -> ToolResult {
    let (status, value) = api(state, method, uri, body).await;
    if status < 400 {
        Ok(value)
    } else {
        Err(None)
    }
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A query string from the arguments that are present (lists repeat).
fn query(pairs: &[(&str, Option<&Value>)]) -> String {
    let mut parts = Vec::new();
    for (key, value) in pairs {
        match value {
            None | Some(Value::Null) => {}
            Some(Value::Array(items)) => parts.extend(items.iter().map(|item| format!("{key}={}", encode(&scalar(item))))),
            Some(value) => parts.push(format!("{key}={}", encode(&scalar(value)))),
        }
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "true" } else { "false" }.into(),
        other => other.to_string(),
    }
}

fn int_arg(arguments: &Value, key: &str) -> Result<Option<i64>, Option<String>> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) if n.is_i64() => Ok(n.as_i64()),
        Some(Value::Number(n)) if n.as_f64().is_some_and(|x| x.fract() == 0.0) => Ok(n.as_f64().map(|x| x as i64)),
        Some(Value::String(s)) => s.trim().parse().map(Some).map_err(|_| Some(format!("{key}: Input should be a valid integer"))),
        Some(_) => Err(Some(format!("{key}: Input should be a valid integer"))),
    }
}

fn required_int(arguments: &Value, key: &str) -> Result<i64, Option<String>> {
    int_arg(arguments, key)?.ok_or_else(|| Some(format!("{key}: Field required")))
}

fn track_list(items: Value) -> Value {
    let items = match items {
        Value::Array(items) => items,
        _ => Vec::new(),
    };
    json!({"count": items.len(), "tracks": items})
}

fn arg<'a>(arguments: &'a Value, key: &str) -> Option<&'a Value> {
    arguments.get(key).filter(|v| !v.is_null())
}

fn now_seconds(state: &AppState) -> f64 {
    state.clock.epoch_seconds()
}

async fn setlist_tracks(state: &AppState, id: i64) -> ToolResult {
    Ok(track_list(ok(state, Method::GET, &format!("/api/setlists/{id}/tracks"), None).await?))
}

async fn run(state: &AppState, name: &str, a: &Value) -> ToolResult {
    match name {
        // ---- tracks
        "search_tracks" => {
            let limit = int_arg(a, "limit")?.unwrap_or(50);
            let offset = int_arg(a, "offset")?.unwrap_or(0);
            if !(1..=500).contains(&limit) || offset < 0 {
                return Err(None);
            }
            let keys = [
                "q", "title", "artist", "album", "genres", "subgenres", "key", "bpm", "bpm_range", "min_duration", "max_duration", "min_energy",
                "max_energy", "min_danceability", "max_danceability", "min_brightness", "max_brightness", "min_year", "max_year", "year_status",
                "lyrics_status", "lyrics", "status", "limit", "offset",
            ];
            // The tool's own defaults (limit 50), not the HTTP route's.
            let (limit_value, offset_value) = (json!(limit), json!(offset));
            let pairs: Vec<(&str, Option<&Value>)> = keys
                .iter()
                .map(|k| match *k {
                    "limit" => (*k, Some(&limit_value)),
                    "offset" => (*k, Some(&offset_value)),
                    _ => (*k, a.get(*k)),
                })
                .collect();
            let page = ok(state, Method::GET, &format!("/api/tracks/page{}", query(&pairs)), None).await?;
            let items = page["items"].as_array().cloned().unwrap_or_default();
            Ok(json!({
                "count": items.len(), "tracks": items, "total": page["total"], "limit": page["limit"],
                "offset": page["offset"], "has_more": page["has_more"],
            }))
        }
        "get_track_similar" => {
            let id = required_int(a, "track_id")?;
            let limit = int_arg(a, "limit")?.unwrap_or(20);
            // TrackAppService.get_similar_tracks: the HTTP route's search; the
            // tool serializes the model_dump dicts (no `lyrics`).
            let (status, mut value) = api(state, Method::GET, &format!("/api/tracks/{id}/similar?limit={limit}"), None).await;
            if status >= 400 {
                // A ValueError (or any non-ToolError) is reported without its text.
                return Err(None);
            }
            for item in value.as_array_mut().into_iter().flatten() {
                if let Some(map) = item.as_object_mut() {
                    map.remove("lyrics");
                }
            }
            Ok(track_list(value))
        }
        "suggest_track_genre" => ok(state, Method::GET, &format!("/api/tracks/{}/suggest-genre", required_int(a, "track_id")?), None).await,
        "update_track_genre" => {
            let id = required_int(a, "track_id")?;
            let (status, value) = api(state, Method::PATCH, &format!("/api/tracks/{id}/genre"), Some(json!({"genre": a.get("genre")}))).await;
            match status {
                404 => Err(Some(format!("Track {id} not found"))),
                s if s >= 400 => Err(None),
                _ => Ok(value),
            }
        }
        "update_track_info" => {
            let id = required_int(a, "track_id")?;
            let mut body = Map::new();
            for key in ["title", "artist", "album", "year"] {
                if let Some(value) = arg(a, key) {
                    body.insert(key.into(), value.clone());
                }
            }
            ok(state, Method::PATCH, &format!("/api/tracks/{id}/info"), Some(Value::Object(body))).await
        }
        // ---- setlists
        "list_setlists" => Ok(json!({"setlists": ok(state, Method::GET, "/api/setlists", None).await?})),
        "create_setlist" => ok(state, Method::POST, "/api/setlists", Some(json!({"name": a.get("name")}))).await,
        "rename_setlist" => {
            let id = required_int(a, "setlist_id")?;
            ok(state, Method::PUT, &format!("/api/setlists/{id}"), Some(json!({"name": a.get("name")}))).await
        }
        "delete_setlist" => {
            ok(state, Method::DELETE, &format!("/api/setlists/{}", required_int(a, "setlist_id")?), None).await?;
            Ok(json!({"ok": true}))
        }
        "get_setlist_tracks" => setlist_tracks(state, required_int(a, "setlist_id")?).await,
        "set_setlist_tracks" => {
            let id = required_int(a, "setlist_id")?;
            let (ids, data) = (arg(a, "track_ids"), arg(a, "track_data"));
            if ids.is_some() == data.is_some() {
                return Err(None);
            }
            let entries: Vec<Value> = match (ids, data) {
                (Some(Value::Array(ids)), _) => ids.clone(),
                (_, Some(Value::Array(items))) => {
                    let mut entries = Vec::new();
                    for item in items {
                        if !item["id"].is_i64() {
                            return Err(None);
                        }
                        let annotation = item.get("wordplay_json").cloned().unwrap_or(Value::Null);
                        if let Some(text) = annotation.as_str() {
                            if !serde_json::from_str::<Value>(text).is_ok_and(|v| v.is_object()) {
                                return Err(None);
                            }
                        } else if !annotation.is_null() {
                            return Err(None);
                        }
                        entries.push(json!({"id": item["id"], "wordplay_json": annotation}));
                    }
                    entries
                }
                _ => return Err(None),
            };
            ok(state, Method::POST, &format!("/api/setlists/{id}/tracks"), Some(Value::Array(entries))).await?;
            setlist_tracks(state, id).await
        }
        "add_track_to_setlist" => {
            let id = required_int(a, "setlist_id")?;
            let body = json!({"track_id": required_int(a, "track_id")?, "position": int_arg(a, "position")?});
            ok(state, Method::POST, &format!("/api/play/playlists/{id}/tracks"), Some(body)).await?;
            setlist_tracks(state, id).await
        }
        "remove_track_from_setlist" => {
            let id = required_int(a, "setlist_id")?;
            let track = required_int(a, "track_id")?;
            let current = ok(state, Method::GET, &format!("/api/setlists/{id}/tracks"), None).await?;
            if let Some(entry) = current.as_array().and_then(|items| items.iter().find(|item| item["id"].as_i64() == Some(track))) {
                let entry = entry["setlist_track_id"].as_i64().unwrap_or_default();
                let _ = api(state, Method::DELETE, &format!("/api/play/playlists/{id}/tracks/{entry}"), None).await;
            }
            setlist_tracks(state, id).await
        }
        "recommend_next_track" | "recommend_next_track_page" => {
            let page = name.ends_with("_page");
            let id = required_int(a, "track_id")?;
            let limit = int_arg(a, "limit")?.unwrap_or(if page { 100 } else { 10 });
            let offset = int_arg(a, "offset")?.unwrap_or(0);
            if page && (!(1..=500).contains(&limit) || offset < 0) {
                return Err(None);
            }
            let targets = ["target_bpm", "target_energy", "target_danceability", "target_brightness", "target_noisiness", "year_min", "year_max"];
            let has_targets = targets.iter().any(|k| arg(a, k).is_some());
            let limit_value = json!(limit);
            let offset_value = json!(offset);
            let id_value = json!(id);
            let mut pairs: Vec<(&str, Option<&Value>)> =
                vec![("track_id", Some(&id_value)), ("limit", Some(&limit_value)), ("genres", a.get("genres")), ("subgenres", a.get("subgenres"))];
            if page || has_targets {
                pairs.push(("offset", Some(&offset_value)));
                pairs.extend(targets.iter().map(|k| (*k, a.get(*k))));
                let mut result = ok(state, Method::GET, &format!("/api/recommendations/next/page{}", query(&pairs)), None).await?;
                if arg(a, "camelot").and_then(Value::as_bool).unwrap_or(false) {
                    let base = track_key(state, id).await;
                    result["items"] = Value::Array(key_filter(result["items"].as_array().cloned().unwrap_or_default(), base.as_deref()));
                }
                if page {
                    return Ok(result);
                }
                return Ok(track_list(result["items"].clone()));
            }
            let mut items = ok(state, Method::GET, &format!("/api/recommendations/next{}", query(&pairs)), None).await?;
            if arg(a, "camelot").and_then(Value::as_bool).unwrap_or(false) {
                let base = track_key(state, id).await;
                items = Value::Array(key_filter(items.as_array().cloned().unwrap_or_default(), base.as_deref()));
            }
            Ok(track_list(items))
        }
        "generate_auto_setlist" => {
            let body = json!({
                "limit": a.get("length"), "min_length": a.get("min_length"), "max_length": a.get("max_length"),
                "seed_track_ids": a.get("seed_track_ids"), "genres": a.get("genres"), "subgenres": a.get("subgenres"),
            });
            Ok(track_list(ok(state, Method::POST, "/api/recommendations/auto", Some(body)).await?))
        }
        "generate_bridge_setlist" => {
            let body = json!({
                "start_track_id": required_int(a, "start_track_id")?, "end_track_id": required_int(a, "end_track_id")?,
                "length": int_arg(a, "length")?.unwrap_or(5), "genres": a.get("genres"), "subgenres": a.get("subgenres"),
            });
            Ok(track_list(ok(state, Method::POST, "/api/recommendations/path", Some(body)).await?))
        }
        "export_setlist_m3u8" => {
            let id = required_int(a, "setlist_id")?;
            let content = ok(state, Method::GET, &format!("/api/setlists/{id}/export/m3u8"), None).await?;
            Ok(json!({"setlist_id": id, "m3u8": content}))
        }
        "validate_setlist_export" => ok(state, Method::GET, &format!("/api/setlists/{}/export/validate", required_int(a, "setlist_id")?), None).await,
        "add_track_to_setlist_with_wordplay" => {
            let id = required_int(a, "setlist_id")?;
            let track = required_int(a, "track_id")?;
            let current = ok(state, Method::GET, &format!("/api/setlists/{id}/tracks"), None).await?;
            let mut data: Vec<Value> = current
                .as_array()
                .map(|items| items.iter().map(|t| json!({"id": t["id"], "wordplay_json": t.get("wordplay_json").cloned().unwrap_or(Value::Null)})).collect())
                .unwrap_or_default();
            let mut wordplay = Map::new();
            for key in [
                "keyword", "source_phrase", "target_phrase", "source_cue_mode", "from_timestamp", "source_cue_end_timestamp", "to_timestamp",
                "target_intro_timestamp", "target_landing_timestamp",
            ] {
                if let Some(value) = arg(a, key) {
                    // Optional[float] parameters: pydantic hands json.dumps a float (20 -> 20.0).
                    let value = match value {
                        Value::Number(n) if key.ends_with("_timestamp") => n.as_f64().map_or_else(|| value.clone(), |x| json!(x)),
                        Value::String(text) if key.ends_with("_timestamp") => {
                            plumdeck_core::compat::text::py_strip(text).parse::<f64>().map_or_else(|_| value.clone(), |x| json!(x))
                        }
                        _ => value.clone(),
                    };
                    wordplay.insert(key.into(), value);
                }
            }
            let mut entry = json!({"id": track});
            if !wordplay.is_empty() {
                entry["wordplay_json"] = json!(python_dumps(&Value::Object(wordplay)));
            }
            match int_arg(a, "position")? {
                None => data.push(entry),
                Some(position) => {
                    let at = position.clamp(0, data.len() as i64) as usize;
                    data.insert(at, entry);
                }
            }
            let _ = api(state, Method::POST, &format!("/api/setlists/{id}/tracks"), Some(Value::Array(data))).await;
            setlist_tracks(state, id).await
        }
        "update_setlist_track_wordplay" => {
            let id = required_int(a, "setlist_track_id")?;
            let text = python_dumps(a.get("wordplay").unwrap_or(&Value::Null));
            let (status, value) = api(state, Method::PATCH, &format!("/api/setlist-tracks/{id}/wordplay"), Some(json!({"wordplay_json": text}))).await;
            if status >= 400 {
                return Err(None);
            }
            Ok(value)
        }
        "clear_setlist_track_wordplay" => {
            let id = required_int(a, "setlist_track_id")?;
            let (status, _) = api(state, Method::PATCH, &format!("/api/setlist-tracks/{id}/wordplay"), Some(json!({"wordplay_json": null}))).await;
            if status >= 400 {
                return Err(None);
            }
            Ok(json!({"ok": true}))
        }
        // ---- genres
        "list_genres" => Ok(json!({"genres": ok(state, Method::GET, "/api/genres/list", None).await?})),
        "list_subgenres" => Ok(json!({"subgenres": ok(state, Method::GET, "/api/genres/subgenres", None).await?})),
        "get_unknown_genre_tracks" => {
            let pairs = [("offset", a.get("offset")), ("limit", a.get("limit")), ("mode", a.get("mode"))];
            let mut items = ok(state, Method::GET, &format!("/api/genres/unknown{}", query(&pairs)), None).await?;
            // serialize(Track): the table's columns, not the HTTP TrackRead extras.
            for item in items.as_array_mut().into_iter().flatten() {
                if let Some(map) = item.as_object_mut() {
                    for key in ["is_analyzed", "lyrics", "has_lyrics"] {
                        map.remove(key);
                    }
                }
            }
            Ok(track_list(items))
        }
        "get_genre_analysis_context" => genre_context(state, a).await,
        "apply_genre_analysis" => {
            let id = required_int(a, "track_id")?;
            let response = analysis_response(state, id, a).await?;
            let body = json!({
                "analyses": [{"track_id": id, "genre": a.get("genre"), "subgenre": a.get("subgenre"),
                    "confidence": a.get("confidence").cloned().unwrap_or(json!("Medium")), "reason": a.get("reason").cloned().unwrap_or(json!(""))}],
                "mode": a.get("mode").cloned().unwrap_or(json!("both")), "overwrite": a.get("overwrite").cloned().unwrap_or(json!(false)),
            });
            ok(state, Method::POST, "/api/genres/apply-analyses", Some(body)).await?;
            Ok(json!({"track_id": id, "analysis": response}))
        }
        "apply_genre_analyses" => {
            let body = json!({
                "analyses": a.get("analyses"), "mode": a.get("mode").cloned().unwrap_or(json!("both")),
                "overwrite": a.get("overwrite").cloned().unwrap_or(json!(false)),
            });
            let result = ok(state, Method::POST, "/api/genres/apply-analyses", Some(body)).await?;
            Ok(json!({"results": result.get("results").cloned().unwrap_or(result)}))
        }
        "get_genre_cleanup_suggestions" => {
            let pairs = [("mode", a.get("mode"))];
            Ok(json!({"groups": ok(state, Method::GET, &format!("/api/genres/cleanup-suggestions{}", query(&pairs)), None).await?}))
        }
        "cleanup_genre_labels" => {
            let body = json!({"target_genre": a.get("target_genre"), "track_ids": a.get("track_ids"), "mode": a.get("mode").cloned().unwrap_or(json!("genre"))});
            ok(state, Method::POST, "/api/genres/cleanup-execute", Some(body)).await
        }
        "get_similar_genre_suggestions" => {
            let id = required_int(a, "track_id")?;
            let pairs = [("threshold", a.get("threshold"))];
            Ok(json!({"suggestions": ok(state, Method::GET, &format!("/api/genres/grouped-suggestions/{id}{}", query(&pairs)), None).await?}))
        }
        "apply_genres_to_files" => ok(state, Method::POST, "/api/genres/apply-to-files", Some(json!({"track_ids": a.get("track_ids")}))).await,
        // ---- lyrics
        "get_track_lyrics" => {
            let id = required_int(a, "track_id")?;
            let (status, mut value) = api(state, Method::GET, &format!("/api/tracks/{id}/lyrics"), None).await;
            if status == 404 {
                return Err(None); // ValueError("Lyrics not found ..."): text hidden
            }
            if status >= 400 {
                return Err(None);
            }
            // serialize(Lyrics): every column, the keyword cache included.
            let db = state.db.clone();
            let cache = state
                .blocking_result(move || -> Result<Vec<plumdeck_core::compat::pyjson::PyValue>, plumdeck_db::DbError> {
                    let conn = db.lease()?;
                    plumdeck_db::repos::rows::py_rows(
                        &conn,
                        "SELECT keywords_json, keywords_content_hash FROM lyrics WHERE track_id = ?",
                        vec![plumdeck_db::duckdb::types::Value::BigInt(id)],
                    )
                })
                .await
                .map_err(|_| None)?
                .map_err(|_| None)?;
            if let (Some(map), Some(row)) = (value.as_object_mut(), cache.first()) {
                if let Value::Object(extra) = py_to_json(row) {
                    map.extend(extra);
                }
            }
            Ok(value)
        }
        "search_lyrics" => {
            let pairs = [("q", a.get("q")), ("exclude_track_id", a.get("exclude_track_id"))];
            Ok(json!({"results": ok(state, Method::GET, &format!("/api/lyrics/search{}", query(&pairs)), None).await?}))
        }
        "find_wordplay_links" => {
            let id = required_int(a, "track_id")?;
            let limit = int_arg(a, "limit")?.unwrap_or(20).max(0) as usize;
            let mut keywords: Vec<String> = Vec::new();
            let mut links = Vec::new();
            for keyword in a["keywords"].as_array().cloned().unwrap_or_default() {
                let keyword = plumdeck_core::compat::text::py_strip(keyword.as_str().unwrap_or("")).to_owned();
                if keyword.chars().count() < 3 || keywords.iter().any(|k| k.to_lowercase() == keyword.to_lowercase()) {
                    continue;
                }
                keywords.push(keyword.clone());
                let id_value = json!(id);
                let keyword_value = json!(keyword);
                let pairs = [("q", Some(&keyword_value)), ("exclude_track_id", Some(&id_value))];
                let results = ok(state, Method::GET, &format!("/api/lyrics/search{}", query(&pairs)), None).await?;
                for r in results.as_array().cloned().unwrap_or_default().into_iter().take(limit) {
                    links.push(json!({"keyword": keyword, "track": r["track"], "snippet": r["snippet"], "matched_text": r["matched_text"], "timestamp": r["timestamp"]}));
                }
            }
            Ok(json!({"track_id": id, "keywords": keywords, "links": links}))
        }
        "register_track_lyrics" => {
            let item = json!({
                "track_id": a.get("track_id"), "content": a.get("content"),
                "source": a.get("source").cloned().unwrap_or(json!("user")), "language": a.get("language"),
            });
            let overwrite = a.get("overwrite").and_then(Value::as_bool).unwrap_or(false);
            let result = register_lyrics(state, vec![item], overwrite).await?;
            Ok(result["results"][0].clone())
        }
        "register_track_lyrics_batch" => {
            let Some(Value::Array(items)) = a.get("items") else { return Err(Some("items: Field required".into())) };
            let overwrite = a.get("overwrite").and_then(Value::as_bool).unwrap_or(false);
            register_lyrics(state, items.clone(), overwrite).await
        }
        // ---- wordplay
        "list_wordplay_pairs" => {
            let pairs = [
                ("status", a.get("status")), ("from_track_id", a.get("from_track_id")), ("query", a.get("query")),
                ("limit", a.get("limit")), ("offset", a.get("offset")),
            ];
            ok(state, Method::GET, &format!("/api/wordplay-pairs{}", query(&pairs)), None).await
        }
        "propose_wordplay_pairs" => {
            let pairs = a["pairs"].as_array().cloned().unwrap_or_default();
            if pairs.is_empty() || pairs.len() > 50 {
                return Err(None);
            }
            propose(state, pairs).await
        }
        "approve_wordplay_pair" => {
            let id = required_int(a, "pair_id")?;
            ok(state, Method::POST, &format!("/api/wordplay-pairs/{id}/approve"), None).await
        }
        "reject_wordplay_pair" => {
            let id = required_int(a, "pair_id")?;
            ok(state, Method::DELETE, &format!("/api/wordplay-pairs/{id}"), None).await?;
            Ok(json!({"ok": true}))
        }
        // ---- assist
        "assist_get_state" => {
            let window = state.assist.window(now_seconds(state));
            let mut settings = window["state"].as_object().cloned().unwrap_or_default();
            let exclusions = settings.remove("exclude_track_ids").and_then(|v| v.as_array().map(Vec::len)).unwrap_or(0);
            settings.insert("excluded_track_count".into(), json!(exclusions));
            let source = track_summary(state, settings.get("source_track_id").and_then(Value::as_i64)).await;
            let presets: Map<String, Value> = plumdeck_domain::assist::presets()
                .iter()
                .map(|p| (p.name.to_owned(), json!(format!("{}（{}）", p.label, p.detail))))
                .collect();
            Ok(json!({
                "window_open": window["open"], "reported_seconds_ago": window["reported_seconds_ago"], "source_track": source,
                "settings": settings, "presets": presets, "agent_list": window["agent_list"],
            }))
        }
        "assist_recommend" | "assist_search" => assist_rank(state, name, a).await,
        "assist_route" => {
            let window = state.assist.window(now_seconds(state));
            let s = &window["state"];
            let source = int_arg(a, "source_track_id")?.or_else(|| s["source_track_id"].as_i64()).filter(|id| *id != 0).ok_or(None)?;
            let scope = arg(a, "genre_scope").or(s.get("genre_scope").filter(|v| !v.is_null())).cloned().unwrap_or(json!("any"));
            if !["any", "same_genre", "same_subgenre"].contains(&scope.as_str().unwrap_or("")) {
                return Err(None);
            }
            let body = json!({
                "source_track_id": source, "target_track_id": required_int(a, "target_track_id")?,
                "intent": arg(a, "intent").or(s.get("intent").filter(|v| !v.is_null())).cloned().unwrap_or(json!("keep")),
                "transition": arg(a, "transition").cloned().unwrap_or(json!(s["transition"].as_bool().unwrap_or(false))),
                "max_intermediate": int_arg(a, "max_intermediate")?.unwrap_or(2).clamp(0, 4),
                "limit": int_arg(a, "limit")?.unwrap_or(3).clamp(1, 5),
                "exclude_track_ids": s.get("exclude_track_ids").cloned().unwrap_or(json!([])),
                "genre_scope": scope,
                "filters": arg(a, "filters").or(s.get("filters").filter(|v| !v.is_null())).cloned().unwrap_or(json!({})),
                "tempo_tolerance": s.get("tempo_tolerance").cloned().unwrap_or(Value::Null),
            });
            let now = state.clock.now_naive();
            let db = state.db.clone();
            let mut result = state
                .blocking_result(move || db.lease().map_err(|_| None).and_then(|conn| assist::mcp_route(&conn, &body, now).map_err(|_| None)))
                .await
                .map_err(|_| None)??;
            if let Some(routes) = result["routes"].as_array_mut() {
                for route in routes {
                    if let Some(tracks) = route["tracks"].as_array_mut() {
                        *tracks = tracks.iter().map(brief).collect();
                    }
                }
            }
            result["mode"] = json!("route");
            Ok(result)
        }
        "assist_apply_settings" => {
            let intent = match arg(a, "intent").and_then(Value::as_str) {
                Some(intent) => Some(plumdeck_domain::assist::resolve_intent(intent).map_err(|_| None)?),
                None => None,
            };
            let scope = arg(a, "genre_scope").cloned();
            if scope.as_ref().is_some_and(|s| !["any", "same_genre", "same_subgenre"].contains(&s.as_str().unwrap_or(""))) {
                return Err(None);
            }
            let mut settings = Map::new();
            settings.insert("intent".into(), json!(intent));
            settings.insert("transition".into(), arg(a, "transition").cloned().unwrap_or(Value::Null));
            settings.insert("genre_scope".into(), scope.unwrap_or(Value::Null));
            settings.insert("limit".into(), int_arg(a, "limit")?.map_or(Value::Null, |l| json!(l.clamp(1, 50))));
            if let Some(filters) = a.get("filters").filter(|v| !v.is_null()) {
                settings.insert("filters".into(), assist::normalized_filters(filters).map_err(|_| None)?);
            }
            let agent = arg(a, "agent_name").and_then(Value::as_str).map(str::to_owned);
            let applied = state.assist.apply_settings(settings, agent, now_seconds(state));
            let open = state.assist.window(now_seconds(state))["open"].as_bool().unwrap_or(false);
            Ok(json!({"applied": applied, "window_open": open, "hint": if open { Value::Null } else { json!("The assist window is not open; it will apply these when opened.") }}))
        }
        "assist_show_tracks" => {
            let window = state.assist.window(now_seconds(state));
            let s = window["state"].clone();
            let mut picks: Vec<(i64, String)> = Vec::new();
            for item in a["tracks"].as_array().cloned().unwrap_or_default().into_iter().take(50) {
                let id = item.get("track_id").and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|t| t.trim().parse().ok()))).ok_or(None)?;
                let reason: String = plumdeck_core::compat::text::py_strip(&scalar(item.get("reason").unwrap_or(&json!("")))).chars().take(400).collect();
                picks.push((id, if item.get("reason").is_none_or(Value::is_null) { String::new() } else { reason }));
            }
            let ids: Vec<i64> = picks.iter().map(|p| p.0).collect();
            let source = s["source_track_id"].as_i64();
            let intent = s["intent"].as_str().unwrap_or("keep").to_owned();
            let now = state.clock.now_naive();
            let db = state.db.clone();
            let checked = state
                .blocking(move || assist::describe_tracks(&*db.lease()?, &ids, source, &intent, now))
                .await
                .map_err(|_| None)?;
            let reasons: std::collections::HashMap<i64, String> = picks.into_iter().collect();
            let shown: Vec<Value> = checked["tracks"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|mut track| {
                    let reason = track["id"].as_i64().and_then(|id| reasons.get(&id)).filter(|r| !r.is_empty()).cloned();
                    track["agent_reason"] = json!(reason);
                    track
                })
                .collect();
            let shown_ids: Vec<Value> = shown.iter().map(|t| t["id"].clone()).collect();
            let rejected = checked["rejected"].as_array().cloned().unwrap_or_default();
            let agent = arg(a, "agent_name").and_then(Value::as_str).map(str::to_owned);
            let listed = state.assist.show_list(
                arg(a, "title").and_then(Value::as_str).unwrap_or(""),
                shown,
                rejected.clone(),
                arg(a, "note").and_then(Value::as_str),
                agent,
                now_seconds(state),
            );
            let open = state.assist.window(now_seconds(state))["open"].clone();
            Ok(json!({"shown_track_ids": shown_ids, "rejected": rejected, "window_open": open, "revision": listed["revision"]}))
        }
        "assist_clear_tracks" => {
            state.assist.dismiss_list();
            Ok(json!({"cleared": true}))
        }
        // ---- set planning
        "list_rekordbox_playlists" => {
            let depth = int_arg(a, "depth")?.unwrap_or(2);
            if !(1..=6).contains(&depth) {
                return Err(None);
            }
            let under = arg(a, "under").and_then(Value::as_str).map(str::to_owned);
            state
                .blocking_result(move || crate::routes::set_planner::list_playlists(under.as_deref(), depth))
                .await
                .map_err(|_| None)?
                .map_err(|_| None)
        }
        "get_rekordbox_playlist_tracks" => {
            let playlist = arg(a, "playlist").map(scalar).ok_or(None)?;
            let recursive = arg(a, "recursive").and_then(Value::as_bool).unwrap_or(true);
            let db = state.db.clone();
            state
                .blocking_result(move || {
                    let conn = db.lease().map_err(|e| e.to_string())?;
                    crate::routes::set_planner::playlist_tracks(&conn, &playlist, recursive)
                })
                .await
                .map_err(|_| None)?
                .map_err(|_| None)
        }
        "plan_phased_setlist" => {
            let phases = a["phases"].as_array().cloned().ok_or(None)?;
            let list = |key: &str| a[key].as_array().cloned().unwrap_or_default();
            let options = crate::routes::set_planner::PlanOptions {
                exclude_track_ids: list("exclude_track_ids").iter().filter_map(Value::as_i64).collect(),
                exclude_artists: list("exclude_artists").iter().filter_map(|v| v.as_str().map(str::to_owned)).filter(|s| !s.is_empty()).collect(),
                opening_track_id: int_arg(a, "opening_track_id")?,
                include_wordplay_targets: arg(a, "include_wordplay_targets").and_then(Value::as_bool).unwrap_or(true),
                wordplay_bonus: arg(a, "wordplay_bonus").and_then(Value::as_f64).unwrap_or(0.25),
                save_as_setlist_name: arg(a, "save_as_setlist_name").and_then(Value::as_str).map(str::to_owned),
            };
            let now = state.clock.now_naive();
            let db = state.db.clone();
            let mut result = state
                .blocking_result(move || {
                    let conn = db.lease().map_err(|e| e.to_string())?;
                    let lookup = |seed: i64, limit: i64| -> Option<Vec<i64>> {
                        // TrackRepository.get_similar_tracks: DuckDB's cosine order, NaN rows included.
                        plumdeck_db::repos::tracks::similar_tracks(&conn, plumdeck_db::repos::tracks::BigInt(i128::from(seed)), plumdeck_db::repos::tracks::BigInt(i128::from(limit)))
                            .ok()
                            .map(|hits| hits.iter().map(|hit| hit.track.id).collect())
                    };
                    crate::routes::set_planner::plan(&conn, &phases, options, &lookup, now)
                })
                .await
                .map_err(|_| None)?
                .map_err(|_| None)?;
            if let Some(id) = result["saved_setlist"]["id"].as_i64() {
                let data = result["track_data"].clone();
                ok(state, Method::POST, &format!("/api/setlists/{id}/tracks"), Some(data)).await?;
            }
            if result.get("saved_setlist").is_none() {
                result.as_object_mut().map(|m| m.remove("saved_setlist"));
            }
            Ok(result)
        }
        // ---- Junction (through the desktop port)
        name if name.starts_with("junction_") => crate::routes::junction::call(state.junction.as_ref(), name, a).await.map_err(Some),
        // ---- analysis jobs
        "plan_track_analysis" | "start_track_analysis" | "get_track_analysis_status" | "pause_track_analysis" | "resume_track_analysis" => {
            crate::routes::ingest::mcp_tool(state, name, a).await.map_err(Some)
        }
        _ => Err(Some("この機能はまだ Rust バックエンドに移植されていません".into())),
    }
}


/// RB-06 (`camelot: true`): same, ±1 and relative keys only, same first,
/// unknown keys last; the scores are unchanged.
fn key_filter(items: Vec<Value>, base: Option<&str>) -> Vec<Value> {
    let Some(base) = base.and_then(plumdeck_key::parse_key) else { return items };
    let rank = |item: &Value| match item["key"].as_str().and_then(plumdeck_key::parse_key) {
        None => Some(2),
        Some(key) => match plumdeck_key::relation(base, key) {
            plumdeck_key::Relation::Same => Some(0),
            plumdeck_key::Relation::Adjacent | plumdeck_key::Relation::Relative => Some(1),
            _ => None,
        },
    };
    let mut ranked: Vec<(usize, usize, Value)> = items.into_iter().enumerate().filter_map(|(index, item)| rank(&item).map(|r| (r, index, item))).collect();
    ranked.sort_by_key(|(r, index, _)| (*r, *index));
    ranked.into_iter().map(|(_, _, item)| item).collect()
}

async fn track_key(state: &AppState, id: i64) -> Option<String> {
    let db = state.db.clone();
    state.blocking(move || plumdeck_db::repos::tracks::get_track(&*db.lease()?, plumdeck_db::repos::tracks::BigInt(id.into()))).await.ok().flatten().and_then(|t| t.key)
}

/// `json.dumps(value, ensure_ascii=False)` (", " and ": " separators).
fn python_dumps(value: &Value) -> String {
    match plumdeck_core::compat::pyjson_loads::loads(&value.to_string()) {
        Ok(parsed) => plumdeck_core::compat::pyjson::dumps(&parsed, &plumdeck_core::compat::pyjson::DumpOptions::unicode()).unwrap_or_default(),
        Err(_) => value.to_string(),
    }
}

const BRIEF_FIELDS: [&str; 13] = ["id", "title", "artist", "bpm", "key", "genre", "subgenre", "year", "energy", "danceability", "brightness", "score", "summary"];

/// `_brief`.
fn brief(candidate: &Value) -> Value {
    let mut out = Map::new();
    for name in BRIEF_FIELDS {
        if let Some(value) = candidate.get(name) {
            out.insert(name.into(), value.clone());
        }
    }
    let reasons: Vec<Value> = candidate["reasons"].as_array().map(|r| r.iter().map(|x| x["text"].clone()).collect()).unwrap_or_default();
    out.insert("reasons".into(), Value::Array(reasons));
    if candidate.get("like").is_some_and(|l| !l.is_null()) {
        out.insert("like".into(), candidate["like"]["text"].clone());
    }
    if candidate.get("wordplay").is_some_and(|w| !w.is_null()) {
        out.insert("wordplay_keyword".into(), candidate["wordplay"]["keyword"].clone());
    }
    Value::Object(out)
}

async fn track_summary(state: &AppState, id: Option<i64>) -> Value {
    let Some(id) = id.filter(|id| *id != 0) else { return Value::Null };
    let db = state.db.clone();
    let track = state.blocking(move || plumdeck_db::repos::tracks::get_track(&*db.lease()?, plumdeck_db::repos::tracks::BigInt(id.into()))).await;
    match track {
        Ok(Some(track)) => {
            let dump: Map<String, Value> = crate::models::track_dump(&track)
                .into_iter()
                .map(|(k, v)| (k.to_owned(), serde_json::from_str(&plumdeck_core::compat::pyjson::dumps(&v, &plumdeck_core::compat::pyjson::DumpOptions::compact_unicode_default_str()).unwrap_or_default()).unwrap_or(Value::Null)))
                .collect();
            let mut out = Map::new();
            for name in ["id", "title", "artist", "bpm", "key", "genre", "subgenre", "year", "energy", "danceability", "brightness"] {
                out.insert(name.into(), dump.get(name).cloned().unwrap_or(Value::Null));
            }
            Value::Object(out)
        }
        _ => Value::Null,
    }
}

async fn assist_rank(state: &AppState, name: &str, a: &Value) -> ToolResult {
    let window = state.assist.window(now_seconds(state));
    let s = &window["state"];
    let filters = arg(a, "filters").or(s.get("filters").filter(|v| !v.is_null())).cloned().unwrap_or(json!({}));
    let limit = int_arg(a, "limit")?.filter(|l| *l != 0).or_else(|| s["limit"].as_i64()).unwrap_or(12).clamp(1, 50);
    let excluded = s.get("exclude_track_ids").cloned().filter(|v| v.is_array()).unwrap_or(json!([]));
    let source = if name == "assist_recommend" { int_arg(a, "source_track_id")?.or_else(|| s["source_track_id"].as_i64()).filter(|id| *id != 0) } else { None };
    let Some(source) = source else {
        let mut result = ok(state, Method::POST, "/api/assist/search", Some(json!({"filters": filters, "limit": limit, "exclude_track_ids": excluded}))).await?;
        if let Some(candidates) = result["candidates"].as_array_mut() {
            *candidates = candidates.iter().map(brief).collect();
        }
        if name == "assist_recommend" {
            result["mode"] = json!("search");
        }
        return Ok(result);
    };
    let body = json!({
        "source_track_id": source,
        "intent": arg(a, "intent").or(s.get("intent").filter(|v| !v.is_null())).cloned().unwrap_or(json!("keep")),
        "transition": arg(a, "transition").cloned().unwrap_or(json!(s["transition"].as_bool().unwrap_or(false))),
        "limit": limit, "exclude_track_ids": excluded,
        "genre_scope": arg(a, "genre_scope").or(s.get("genre_scope").filter(|v| !v.is_null())).cloned().unwrap_or(json!("any")),
        "filters": filters, "tempo_tolerance": s.get("tempo_tolerance").cloned().unwrap_or(Value::Null),
    });
    let now = state.clock.now_naive();
    let db = state.db.clone();
    let mut result = state
        .blocking_result(move || db.lease().map_err(|_| None).and_then(|conn| assist::mcp_recommend(&conn, &body, now).map_err(|_| None)))
        .await
        .map_err(|_| None)??;
    if arg(a, "camelot").and_then(Value::as_bool).unwrap_or(false) {
        let base = track_key(state, source).await;
        let filtered = key_filter(result["candidates"].as_array().cloned().unwrap_or_default(), base.as_deref());
        result["candidates"] = Value::Array(filtered);
    }
    if let Some(candidates) = result["candidates"].as_array_mut() {
        *candidates = candidates.iter().map(brief).collect();
    }
    if let Some(alternatives) = result["alternatives"].as_array_mut() {
        *alternatives = alternatives.iter().map(|item| json!({"intent": item["intent"], "label": item["label"], "track": brief(&item["track"])})).collect();
    }
    result["mode"] = json!("recommend");
    Ok(result)
}

fn py_to_json(value: &plumdeck_core::compat::pyjson::PyValue) -> Value {
    let text = plumdeck_core::compat::pyjson::dumps(value, &plumdeck_core::compat::pyjson::DumpOptions::compact_unicode_default_str()).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/// `LyricsRegistration`: the pydantic checks of one item, briefly (D-22).
fn lyrics_item(item: &Value) -> Result<(i64, String, String, Option<String>), Option<String>> {
    let track_id = match item.get("track_id") {
        Some(Value::Number(n)) if n.is_i64() => n.as_i64().unwrap_or(0),
        Some(Value::Number(n)) if n.as_f64().is_some_and(|x| x.fract() == 0.0) => n.as_f64().unwrap_or(0.0) as i64,
        Some(Value::String(s)) => s.trim().parse().map_err(|_| Some("track_id: Input should be a valid integer".to_owned()))?,
        None | Some(Value::Null) => return Err(Some("track_id: Field required".into())),
        Some(_) => return Err(Some("track_id: Input should be a valid integer".into())),
    };
    if track_id <= 0 {
        return Err(Some("track_id: Input should be greater than 0".into()));
    }
    let text = |key: &str, default: Option<&str>, max: usize| -> Result<Option<String>, Option<String>> {
        match item.get(key) {
            None => Ok(default.map(str::to_owned)),
            Some(Value::Null) if default.is_none() && key == "language" => Ok(None),
            Some(Value::String(s)) => {
                if s.chars().count() > max {
                    return Err(Some(format!("{key}: String should have at most {max} characters")));
                }
                Ok(Some(s.clone()))
            }
            Some(_) => Err(Some(format!("{key}: Input should be a valid string"))),
        }
    };
    let content = text("content", None, 200_000)?.ok_or_else(|| Some("content: Field required".to_owned()))?;
    let source = text("source", Some("user"), 2048)?.unwrap_or_default();
    let language = text("language", None, 64)?;
    for (key, value) in [("content", &content), ("source", &source)] {
        if value.is_empty() {
            return Err(Some(format!("{key}: String should have at least 1 character")));
        }
        if plumdeck_core::compat::text::py_strip(value).is_empty() {
            return Err(Some(format!("{key}: Value error, must not be blank")));
        }
    }
    Ok((track_id, content, source, language))
}

/// `register_lyrics` (app/services/lyrics_app_service.py): the whole batch
/// is validated before any write; existing lyrics are kept unless
/// `overwrite`.
async fn register_lyrics(state: &AppState, items: Vec<Value>, overwrite: bool) -> ToolResult {
    let mut parsed = Vec::new();
    for item in &items {
        parsed.push(lyrics_item(item)?);
    }
    if parsed.is_empty() || parsed.len() > 100 {
        return Err(None); // ValueError: the SDK hides its text
    }
    let mut seen = std::collections::HashSet::new();
    if !parsed.iter().all(|item| seen.insert(item.0)) {
        return Err(None); // ValueError("Duplicate track IDs are not allowed")
    }
    let db = state.db.clone();
    let now = state.clock.now_naive();
    state
        .blocking_result(move || -> ToolResult {
            use plumdeck_db::duckdb::types::Value as Db;
            let stamp = Db::Timestamp(plumdeck_db::duckdb::types::TimeUnit::Microsecond, now.and_utc().timestamp_micros());
            db.write(|conn| {
                for item in &parsed {
                    if plumdeck_db::repos::rows::scalar_int(conn, "SELECT id FROM tracks WHERE id = ?", vec![Db::BigInt(item.0)])?.is_none() {
                        return Ok(Err(None)); // ValueError("Track N not found")
                    }
                }
                conn.execute_batch("BEGIN TRANSACTION")?;
                let mut results = Vec::new();
                for (track_id, content, source, language) in &parsed {
                    let mut statement = conn.prepare("SELECT content, source, language FROM lyrics WHERE track_id = ?")?;
                    let mut rows = statement.query([Db::BigInt(*track_id)])?;
                    let existing: Option<(Option<String>, Option<String>, Option<String>)> = match rows.next()? {
                        Some(row) => Some((row.get(0)?, row.get(1)?, row.get(2)?)),
                        None => None,
                    };
                    let status = match &existing {
                        Some((Some(c), _, _)) if !plumdeck_core::compat::text::py_strip(c).is_empty() && !overwrite => "skipped_existing",
                        Some((c, s, l)) if c.as_deref() == Some(content.as_str()) && s.as_deref() == Some(source.as_str()) && l == language => "unchanged",
                        Some(_) => "updated",
                        None => "created",
                    };
                    match status {
                        "updated" => {
                            conn.execute(
                                "UPDATE lyrics SET content = ?, source = ?, language = ?, keywords_json = NULL, keywords_content_hash = NULL, updated_at = ? WHERE track_id = ?",
                                plumdeck_db::duckdb::params_from_iter(vec![
                                    Db::Text(content.clone()), Db::Text(source.clone()),
                                    language.clone().map_or(Db::Null, Db::Text), stamp.clone(), Db::BigInt(*track_id),
                                ]),
                            )?;
                        }
                        "created" => {
                            conn.execute(
                                "INSERT INTO lyrics (track_id, content, source, language, keywords_json, keywords_content_hash, created_at, updated_at) VALUES (?, ?, ?, ?, NULL, NULL, ?, ?)",
                                plumdeck_db::duckdb::params_from_iter(vec![
                                    Db::BigInt(*track_id), Db::Text(content.clone()), Db::Text(source.clone()),
                                    language.clone().map_or(Db::Null, Db::Text), stamp.clone(), stamp.clone(),
                                ]),
                            )?;
                        }
                        _ => {}
                    }
                    results.push(json!({"track_id": track_id, "status": status}));
                }
                conn.execute_batch("COMMIT")?;
                Ok(Ok(json!({"count": results.len(), "results": results})))
            })
            .map_err(|_| None)?
        })
        .await
        .map_err(|_| None)?
}

/// `GenreAppService.apply_genre_analysis`'s answer (GenreAnalysisResponse),
/// normalized against the library before the write.
async fn analysis_response(state: &AppState, id: i64, a: &Value) -> ToolResult {
    let db = state.db.clone();
    let genre = a.get("genre").filter(|v| !v.is_null()).map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned));
    let subgenre = a.get("subgenre").filter(|v| !v.is_null()).map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned));
    let confidence = match a.get("confidence") {
        None => "Medium".to_owned(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    };
    let reason = match a.get("reason") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => if *b { "True".into() } else { String::new() },
        Some(other) => other.to_string(),
    };
    state
        .blocking_result(move || -> ToolResult {
            use plumdeck_domain::genre_rules::{label_map, normalize_genre_label, normalize_subgenre_label};
            let conn = db.lease().map_err(|_| None)?;
            let Some(track) = plumdeck_db::repos::tracks::get_track(&conn, plumdeck_db::repos::tracks::BigInt(id as i128)).map_err(|_| None)? else {
                return Err(None); // ValueError("Track not found")
            };
            let genre = match genre {
                Some(value) => normalize_genre_label(&value, &label_map(&plumdeck_db::repos::genres::all_genres(&conn).map_err(|_| None)?)),
                None => track.genre.clone().filter(|g| !g.is_empty()).unwrap_or_else(|| "Unknown".into()),
            };
            let subgenre = match subgenre {
                Some(value) => normalize_subgenre_label(&value, &label_map(&plumdeck_db::repos::genres::all_subgenres(&conn).map_err(|_| None)?)),
                None => track.subgenre.clone().unwrap_or_default(),
            };
            let confidence = py_title(plumdeck_core::compat::text::py_strip(&confidence));
            let confidence = if ["High", "Medium", "Low"].contains(&confidence.as_str()) { confidence } else { "Low".to_owned() };
            let reason = if reason.is_empty() { "Classified by the connected MCP client.".to_owned() } else { reason };
            let reason: String = reason.chars().take(500).collect();
            Ok(json!({"genre": genre, "subgenre": subgenre, "reason": reason, "confidence": confidence}))
        })
        .await
        .map_err(|_| None)?
}

/// `str.title()` for the confidence labels.
fn py_title(text: &str) -> String {
    let mut out = String::new();
    let mut previous_cased = false;
    for c in text.chars() {
        if previous_cased {
            out.extend(c.to_lowercase());
        } else {
            out.extend(c.to_uppercase());
        }
        previous_cased = c.is_alphabetic();
    }
    out
}

const DJ_GENRE_GUIDE: &str = "DJ library taxonomy:\n\
- Use the best-known public/catalog genre for the exact track when the title and artist are recognizable.\n\
- Use one concise main genre and one concise subgenre when available.\n\
- Do not choose a vague umbrella label when a more recognized specific genre is clearly known.\n\
- Do not restrict yourself to any fixed genre list.";

/// `GenreAppService.get_analysis_context`.
async fn genre_context(state: &AppState, a: &Value) -> ToolResult {
    let mode = a.get("mode").and_then(Value::as_str).unwrap_or("both").to_owned();
    if !["genre", "subgenre", "both"].contains(&mode.as_str()) {
        return Err(None); // ValueError: not a valid AnalysisMode
    }
    let ids: Vec<i64> = match a.get("track_ids") {
        Some(Value::Array(items)) if !items.is_empty() => {
            let mut ids = Vec::new();
            for item in items {
                ids.push(item.as_i64().ok_or_else(|| Some("track_ids: Input should be a valid integer".to_owned()))?);
            }
            ids.sort_unstable();
            ids.dedup();
            ids
        }
        _ => {
            let (default_limit, mode_value) = (json!(50), json!(mode));
            let pairs = [("offset", a.get("offset")), ("limit", a.get("limit").or(Some(&default_limit))), ("mode", Some(&mode_value))];
            let items = ok(state, Method::GET, &format!("/api/genres/unknown{}", query(&pairs)), None).await?;
            items.as_array().into_iter().flatten().filter_map(|t| t["id"].as_i64()).collect()
        }
    };
    let genres = ok(state, Method::GET, "/api/genres/list", None).await?;
    let subgenres = ok(state, Method::GET, "/api/genres/subgenres", None).await?;
    let take = |value: &Value, n: usize| Value::Array(value.as_array().map(|v| v.iter().take(n).cloned().collect()).unwrap_or_default());
    let db = state.db.clone();
    let tracks = state
        .blocking_result(move || -> ToolResult {
            let conn = db.lease().map_err(|_| None)?;
            let mut tracks = Vec::new();
            for id in ids {
                let Some(t) = plumdeck_db::repos::tracks::get_track(&conn, plumdeck_db::repos::tracks::BigInt(id as i128)).map_err(|_| None)? else { continue };
                tracks.push(json!({
                    "id": t.id, "title": t.title, "artist": t.artist, "album": t.album, "year": t.year, "bpm": t.bpm, "key": t.key,
                    "energy": t.energy, "danceability": t.danceability, "brightness": t.brightness, "noisiness": t.noisiness,
                    "genre": t.genre, "subgenre": t.subgenre, "is_genre_verified": t.is_genre_verified,
                }));
            }
            Ok(Value::Array(tracks))
        })
        .await
        .map_err(|_| None)??;
    Ok(json!({
        "mode": mode,
        "taxonomy_guide": DJ_GENRE_GUIDE,
        "rules": [
            "Use English genre labels.",
            "Choose one dominant genre and one concise subgenre when applicable.",
            "Do not include edit labels such as Intro, Clean, Dirty, or Extended.",
            "Reuse the library vocabulary when it fits; introduce a label only when needed.",
            "Use confidence High, Medium, or Low. Do not mark uncertain guesses as High.",
        ],
        "existing_genres": take(&genres, 60),
        "existing_subgenres": take(&subgenres, 80),
        "tracks": tracks,
    }))
}

/// `get_similar_tracks`: the library ranked by embedding similarity.
pub fn similar_tracks(conn: &plumdeck_db::duckdb::Connection, id: i64, limit: i64, now: chrono::NaiveDateTime) -> Result<Vec<Value>, ()> {
    let stored = plumdeck_db::repos::recommendations::embedding(conn, id).map_err(|_| ())?;
    let Some((model, Some(text))) = stored else { return Err(()) };
    let Some(vector) = plumdeck_domain::mixability::parse_embedding(Some(&text)) else { return Err(()) };
    let pool = plumdeck_db::repos::recommendations::candidate_pool(
        conn,
        &plumdeck_db::repos::recommendations::PoolQuery {
            targets: Default::default(),
            genres: None,
            subgenres: None,
            limit: i64::MAX / 4,
            exclude_ids: &[id],
            candidate_ids: None,
        },
    )
    .map_err(|_| ())?;
    let mut scored: Vec<(f64, &plumdeck_db::repos::recommendations::PoolRow)> = pool
        .iter()
        .filter_map(|row| {
            plumdeck_domain::mixability::cosine_similarity(Some(&vector), row.vector.as_deref(), model.as_deref(), row.embedding_model.as_deref()).map(|s| (s, row))
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.1.id.cmp(&b.1.id)));
    Ok(scored
        .into_iter()
        .take(limit.max(0) as usize)
        .map(|(similarity, row)| {
            let mut map: Map<String, Value> = crate::routes::setlists::pool_track(row, now)
                .into_iter()
                .map(|(k, v)| (k.to_owned(), serde_json::from_str(&plumdeck_core::compat::pyjson::dumps(&v, &plumdeck_core::compat::pyjson::DumpOptions::compact_unicode_default_str()).unwrap_or_default()).unwrap_or(Value::Null)))
                .collect();
            map.insert("similarity".into(), json!(similarity));
            Value::Object(map)
        })
        .collect())
}

async fn propose(state: &AppState, pairs: Vec<Value>) -> ToolResult {
    let now = state.clock.now_naive();
    let db = state.db.clone();
    state
        .blocking_result(move || -> ToolResult {
            db.write(|conn| {
                conn.execute_batch("BEGIN TRANSACTION")?;
                let mut items = Vec::new();
                for pair in &pairs {
                    let parsed = plumdeck_core::compat::pyjson_loads::loads(&pair.to_string()).map_err(|e| plumdeck_db::DbError::Invalid(e.to_string()))?;
                    let plumdeck_core::compat::pyjson::PyValue::Dict(values) = parsed else {
                        let _ = conn.execute_batch("ROLLBACK");
                        return Ok(Err(None));
                    };
                    let values: Vec<(String, plumdeck_core::compat::pyjson::PyValue)> = values
                        .into_iter()
                        .filter_map(|(k, v)| match k {
                            plumdeck_core::compat::pyjson::PyValue::Str(k) => Some((k, v)),
                            _ => None,
                        })
                        .collect();
                    match crate::routes::wordplay::propose(conn, &values, now) {
                        Ok(item) => items.push(item),
                        Err(_) => {
                            let _ = conn.execute_batch("ROLLBACK");
                            return Ok(Err(None));
                        }
                    }
                }
                conn.execute_batch("COMMIT")?;
                let count = items.len();
                let list = plumdeck_core::compat::pyjson::PyValue::List(items);
                let text = plumdeck_core::compat::pyjson::dumps(&list, &plumdeck_core::compat::pyjson::DumpOptions::compact_unicode_default_str()).unwrap_or_default();
                Ok(Ok(json!({"items": serde_json::from_str::<Value>(&text).unwrap_or(json!([])), "count": count})))
            })
            .map_err(|_| None)?
        })
        .await
        .map_err(|_| None)?
}
