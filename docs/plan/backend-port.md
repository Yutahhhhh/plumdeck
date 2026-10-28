# バックエンド Rust 移行（ポート計画）

Python バックエンド（FastAPI + DuckDB + MCP + ジョブ）を、Tauri プロセス内で動く Rust バックエンドに置き換える計画です。目的とオーナー決定は [README.md](README.md) を見てください。

行番号を含む参照は計画時点のものです。移植前に必ず現在のソースで確かめてください。

---

## 0. 結論

1. **ワークスペース**：`src-tauri/Cargo.toml` をワークスペースのルートにし、新しい crate は `src-tauri/crates/` に置く。`src-tauri/target` と `src-tauri/Cargo.lock` の場所が変わらないので、リリース系のスクリプトとワークフローは変えずに済む。デコード、DSP、ワーカー、エンジン、Junction の crate もここに置き、`Cargo.lock` を 1 つにする（以前の設計にあった `native/rust/` 案はここに畳み込んだ）。
2. **同一プロセス・同一ポート・同一パス**：Tauri プロセスの中で axum サーバを 1 つ動かし、`/`、`/api/*`、`/ws/*`、rmcp の `/mcp` を 8001（dev）/ 48123（prod）で提供する。フロントの `src/services/api-client.ts`、CSP（`tauri.conf.json`）、外部 MCP クライアントの設定は変えない。
3. **段階的な本番切替はできない**：DuckDB は 1 ファイルを書込みで開けるのが 1 プロセスだけ。開発はスライス単位で差分テストにより検証し、本番切替は S13 で一度に行う。1 リリース分は Python へのフォールバックを残す（オーナー判断）。
4. **既定はバグ互換**：応答と DB 書込みを Python と一致させる。意図的な差分は `contracts/parity/deviations.yaml` に載せたものだけ。既知のバグは切替後（S14）に 1 件ずつ直す。Python が 500 を返すケースは「未定義」とし、比較から外す（N12）。
5. **DROP と PORT**：
   - DROP：呼び出し元が無いかテストだけの HTTP ルート、常に空の rekordbox Mirror（4 ルート）、Tauri へ置き換える 2 ルート、固定エラーを返すだけの Junction MCP 5 ツール、Junction の中継層。**加えて Explorer 関連（`POST /api/fs/list`、Explorer の待ち方）はオーナー決定により移植しない。**
   - PORT：残りの HTTP ルートと新規 1 本（`PUT /api/tracks/{id}/lyrics`）、WS 3 本、MCP ツールとプロンプト 2 本、ジョブ 4 系統、起動・停止のシーケンス。
6. **DuckDB**：`duckdb = "=1.10505.0"`（DuckDB 1.5.5）に固定し、既存ファイル（storage v64、WAL が残っている状態）をそのまま開く。時刻は「ローカルの naive 時刻を明示的に bind する」ことで既存データと揃える。
7. **パリティの方式**：Python（オラクル）と Rust を同じ DB の別々のコピーに向けて並走させ、同じ順でリクエストを流し、応答・終了後の DB・副作用ファイルを正規化して比較する。Python 撤去後は、フィクスチャ DB のゴールデンを再生して比較する。
8. **解析への依存は S11 に閉じ込める**。必要なら暫定で、Rust から既存の Python 解析ワーカー（V1 行プロトコル `PLUMDECK_ANALYSIS_WORKER_V1:`）を呼び、本番切替を Rust 解析の完了から切り離す。

---

## 1. crate 構成

### 1.1 ワークスペース

```toml
# src-tauri/Cargo.toml
[workspace]
members  = [".", "crates/plumdeck-*", "crates/tools/*"]
resolver = "2"

[workspace.package]
edition      = "2021"
rust-version = "1.91"

[workspace.dependencies]   # 版は '=' で一元的に固定する

[profile.release]
panic = "unwind"           # handler の panic を 500 に落とすため。abort にしない
```

### 1.2 crate と依存の向き

```
src-tauri/                 package `plumdeck`（Tauri アプリ。S13 から backend を in-process で起動）
src-tauri/crates/
  plumdeck-core/           Python 互換の基本部品、設定とパス、エラー型、時刻、ハッシュ     [済]
  plumdeck-domain/         純関数のドメイン（I/O なし）
  plumdeck-db/             DuckDB ハンドル、ゲート、スキーマ、移行、復元回復、配列コーデック、
                           SQLite キュー、リポジトリ                                         [S1 分済]
  plumdeck-rekordbox/      master.db（SQLCipher）、ANLZ、パス同一性、場所の解決             [RB-10/11 済]
  plumdeck-decode/         ヘッダ解析、symphonia デコード、basis 整列、PcmPages             [W1-04/05 済]
  plumdeck-media/          タグ（lofty）、Range 配信、詳細波形、録音書き出し、リムーバブルデバイス
  plumdeck-services/       アプリサービス層（HTTP と MCP が共有）、ジョブ、WS の状態、AppState、
                           トレイト（Analyzer、JunctionPort、Clock）
  plumdeck-http/           axum ルータ、ミドルウェア、抽出器、エラー整形、WS
  plumdeck-mcp/            rmcp サーバー、ワイヤ互換層
  plumdeck-backend/        起動・停止、サーバ組み立て（lib）と bin `plumdeck-backend`（ヘッドレス）[S0〜S2 済]
  tools/plumdeck-parity/   差分テストランナー、正規化、DB diff、フィクスチャ生成（出荷しない）[済]
  （後続）plumdeck-key / -dsp / -analysis / -embed / -worker / -engine* / junction-*

core → domain → (db, rekordbox, media) → services → (http, mcp) → backend → src-tauri
```

S0〜S2 では HTTP とミドルウェアを `plumdeck-backend` の中に置いています。ルートが増えたら `plumdeck-http` / `plumdeck-services` へ分けてください（分けるかどうかは実装者の判断でよいが、HTTP と MCP がサービス層を共有する形は守る）。

### 1.3 主要モジュール（右は移植元）

```
plumdeck-core      paths.rs ← backend/config.py（platformdirs 互換）
                   config.rs（ポート、env の上書き：PLUMDECK_PORT, DB_PATH, USER_DATA_DIR,
                              PLUMDECK_LOG_DIR, PLUMDECK_REKORDBOX_DB）
                   error.rs（ApiError{status, detail: Text|Json} / ToolFailure）
                   time.rs（local_now_naive, py_isoformat, py_str_datetime）
                   compat/（pyjson, pyfmt, text, pathnorm）、hash.rs
plumdeck-domain    set_duration, audio_math（normalize_key, bpm_distance, mixability, CAMELOT 表）,
                   embedding, setlist_builder, phased_setlist_builder,
                   assist/{presets, scoring, filters, history, text}（日本語の文言は逐語コピー）,
                   wordplay_rules, genre_rules, beat_grid, transition（parse_transition は手書き
                   スキャナ）, csv_rules, metadata_skip
plumdeck-db        handle.rs（差し替え可能な Arc<RwLock<Option<DbInner>>>、try_clone のプール最大 8、
                   MaintenanceGate、write_lock）, schema.rs（contracts の v5 DDL を include_str!、
                   COMPATIBILITY_STATEMENTS は毎回実行）, compaction.rs, restore_recovery.rs,
                   array_codec.rs, jobs_sqlite.rs（rusqlite、同じファイル名とスキーマ）, backup.rs,
                   sqlutil.rs, repos/{tracks, setlists, recommendation, genres, wordplay, lyrics,
                   performance_metadata, grid_candidates, analysis_grid_refresh, ingestion, play,
                   imports, workflows, settings, dashboard}
plumdeck-services  state.rs（AppState）, tracks, setlists, recommendations, genres, wordplay,
                   lyrics（ルーター内のロジックをここへ）, metadata_job, csv, settings, dashboard,
                   performance_metadata, grid_candidates, assist/{service, bridge}, set_planner,
                   workflows/{media_repair, versions, presets, usb, timeline, backup, restore},
                   play, play_import, ingestion, analysis_jobs, background_task, coordinator,
                   engine_profile, junction（trait JunctionPort と引数検証）
plumdeck-mcp       server.rs（name=plumdeck、instructions は contracts/mcp/instructions.txt を逐語）,
                   wire.rs（text は indent=2 の JSON、structuredContent は {"result":…}、
                   エラー文 'Error executing tool X: msg'）, lax.rs（JSON 文字列で来た配列や
                   オブジェクト、数値文字列の受け入れ）, schema_compat.rs, tools/*, prompts.rs
plumdeck-backend   lib.rs（start(cfg, hooks, junction, analyzer) -> BackendHandle、
                   shutdown(deadline)）, startup.rs（settings → services → database → queue → ready）,
                   bin/plumdeck-backend.rs（env は Python と同名。PLUMDECK_STARTUP 行と stdin の
                   'plumdeck:shutdown' も実装し、scripts/smoke-backend.py を流用する）
```

### 1.4 ランタイム・状態・並行性

- **ランタイム**：Tauri の tokio ランタイム上で axum と rmcp を動かす。DuckDB、SQLCipher、rusqlite、ファイルハッシュ、タグ I/O、エンジンへの同期送信は `spawn_blocking` で呼び、blocking pool の上限を明示する。
- **DuckDB の接続**：ルートの `Connection` を 1 本だけ開き、`try_clone` で配る（最大 8）。同じファイルを 2 回 `open` しない（同一プロセス内でもロックが衝突する）。
- **ゲート**：`MaintenanceGate = RwLock<()>`。通常の貸し出しは read、backup / restore は write。write を取れないときは Python と同じ日本語メッセージで拒否する。最大 2 秒だけ待ってから拒否する（D-10）。
- **書込みの直列化**：`write_lock: Mutex<()>`（Python の `db_lock` 相当）。対象は compare-and-write の区間（performance metadata の replace、グリッド候補の upsert、wordplay、解析結果の保存、setlist の差分更新）。DB の貸し出しは SQL を実行する区間だけにする。
- **解析枠（AnalysisCoordinator）**：`Semaphore(1)` に owner ラベルと token。待ち方は「Play 取り込み＝async で待つ」「選択解析と grid-jobs＝即座に拒否（409 / ToolError）」。Explorer の 1 秒ポーリングの待ち方は Explorer 廃止に伴い移植しない（新しい取り込みキューの待ち方は取り込み UX の仕様で決める）。公平性は FIFO。
- **プロセス内シングルトン**（HTTP と MCP が同じ `Arc` を共有）：AssistBridge（revision、30 秒で stale）、HistoryCache、rekordbox ライブラリのキャッシュ（stamp と TTL 300 秒）、解析ジョブのスレッド、Play 取り込みの単一ワーカー、BackgroundTask の `watch::Sender`。
- **決定性の注入口**：`Clock`、`Rng`、`CommandRunner`（diskutil / PowerShell）、`ExternalUrls`（iTunes / LRCLIB）を差し替え可能にし、`--parity` のときだけ差し替え用の実装を有効にする。

### 1.5 HTTP / WS / MCP

- **ルータ**：`/`、`/api`、`/ws`、`/mcp` を nest し、fallback は FastAPI と同じ形の 404 / 405（D-12）。axum 0.8 の `{id}` 構文は FastAPI と同じ。同じパスでメソッドだけ違うもの（`PATCH` と `POST /api/metadata/update`）はメソッドで振り分ける。
- **起動ゲート**：`GET /` は `{"message":"plumdeck Backend API is running"}` を完全一致で返す。listener の bind は DB 初期化の後。bind に失敗したら `startup::fail`（他プロセスは kill しない）。
- **エラー**：業務エラーは `{"detail": "<日本語>"}`、楽観ロックの 409 は `{"detail":{"message":…,"current_revision":n}}`。抽出器の検証エラーは 422 `{"detail":[{type,loc,msg}]}`。panic は `CatchPanicLayer` で 500。
- **ミドルウェア**：CORS は Python の一覧をそのまま。Host / Origin の検証を全ルートに入れる（D-06）。
- **Range 配信**：`/api/stream` と録音の audio は 206 / 416・Accept-Ranges・If-Range に対応。Content-Type は拡張子の固定マップ。録音には `Cache-Control: no-store`。パスの解決順（原文 → NFC → NFD）を維持。
- **WS**：接続時に初回スナップショット。ingest は実行中 3 秒ごとにハートビート。play-imports は購読者がいるときだけプロデューサ 1 本が 2 秒ごとに問い合わせて全員に配る。送信は 2 秒でタイムアウトし、失敗した購読者は切り離す。
- **MCP**：rmcp の `StreamableHttpService` + `LocalSessionManager`、legacy session、SSE（json_response=false）、ボディ上限 4 MiB。allowed hosts / origins を明示的に設定する（rmcp の既定は空）。終了時は CancellationToken でセッションを閉じる。

### 1.6 Tauri 側の変更（S13 でまとめて）

- `src-tauri/src/lib.rs`：Junction の MCP 中継（`junction_mcp_bridge.rs` の TCP、トークン、HTTP パーサ）を削除し、`JunctionPortImpl{app, supervisor}` を `src-tauri/src/junction_port.rs` に作って `dispatch_action`・`export_exchange`・`redact_value` とそのテストを移す。sidecar の起動を `plumdeck_backend::start(...)` に置き換える。イベント `backend://startup`、コマンド `backend_startup_status`、`Progress{total:5}` は維持する。
- 終了時（`RunEvent::Exit`）は、取り込みのキャンセル → メタデータジョブ停止 → 解析ジョブを最大 30 秒 join → `CHECKPOINT` → DB を drop、の順に同期で行い、全体を最大 40 秒で打ち切ってからエンジンを止める。
- `tauri.conf.json` の externalBin から `plumdeck-server` を外し（解析を外部ワーカーで動かすなら `plumdeck-worker` を入れる）、capabilities の sidecar 実行許可を削除する。`tauri-plugin-shell` は他で使っているので残す。
- 開発フロー：`backend-dev` を `cargo run -p plumdeck-backend -- --port 8001` に置き換える。`TAURI_SKIP_SIDECAR` は `TAURI_SKIP_BACKEND` に改名（旧名も受け付ける）。
- フォールバック（切替リリースだけ）：隠し設定 `PLUMDECK_BACKEND=python`。別案として同じバイナリを `plumdeck --backend-server` で子プロセスとして起動する隔離モードも用意できる。

### 1.7 依存クレート

| 用途 | crate（`=` で固定） | 備考 |
|---|---|---|
| DB | `duckdb =1.10505.0`（bundled, json, chrono） | DuckDB 1.5.5。icu を入れると cmake が必須 |
| SQLCipher / SQLite | `rusqlite`（bundled-sqlcipher, backup）。Windows だけ vendored-openssl | master.db と SQLite キューの両方。`links=sqlite3` なので 1 バイナリに 1 つ |
| HTTP | `axum 0.8`（ws）、`tower`、`tower-http`（cors, catch-panic, fs, limit, trace） | tokio は Tauri と同じ版 |
| MCP | `rmcp`（server, macros, schemars, transport-streamable-http-server）、`schemars` | client feature は parity ツールだけ |
| JSON | `serde`、`serde_json`（preserve_order） | キー順の互換 |
| Unicode | `unicode-normalization`、`caseless`、`unicode-general-category`、`same-file` | casefold、NFKC、samefile |
| 正規表現 | `regex`（Python の `\w` との差に注意。必要なら手書き） | |
| タグと音声 | `lofty`、`symphonia`、`hound`、FLAC エンコーダ | ffmpeg と mutagen を撤去 |
| 書庫と XML | `zip`、`sha2`、`hex`、`quick-xml`、`flate2` | |
| OS | `plist`（diskutil）、`windows`（ファイルシステム、IOCTL、Shell） | |
| その他 | `chrono`、`uuid`、`parking_lot`、`arc-swap`、`reqwest`、`tracing` | ログは `<USER_DATA_DIR>/logs/plumdeck.log`（10MB×5） |

すべて MIT / Apache / MPL-2.0 / BSD 系で、所有ライン（D1）に適合する。

### 1.8 Python 互換の基本部品（`plumdeck-core::compat`）

どれも `contracts/compat-vectors/*.jsonl`（CPython で生成）で全件一致を確認する。

| Python の挙動 | 使う箇所 |
|---|---|
| `json.dumps` の各形式（区切り、ensure_ascii、`default=str`、float の repr、キー順） | USB snapshot hash、wordplay_json、grid fingerprint、plan_json / detail_json |
| `datetime.isoformat()` と `str(datetime)` | API 応答、snapshot hash |
| `:g`、`:+.1f`、`%.2f`、`round`（偶数丸め）、`int()`、`//`、`%` | Assist の文言、XML の数値、duration_ms、wordplay の round4 |
| `str.casefold`、NFKC、`split()` / `strip()` の空白集合 | Assist、rekordbox の照合、wordplay の重複判定 |
| `[^\W_]+(?:'[^\W_]+)?`（Python の `\w` は Mn / Mc を含まない） | トラックの全体検索 |
| `Path.resolve().as_uri()` | rekordbox XML の Location |
| NFC / NFD の変種、`ntpath.normpath` / `normcase`、`samefile` | rekordbox のパス同一性、`/api/stream`、CSV |
| f32（DuckDB の FLOAT）を f64 に広げた値の repr | 全 API、スコアリング、snapshot hash |
| `statistics.median`、`np.rint` | PQTZ、詳細波形 |

### 1.9 エンジン依存の仕様の隔離

`engine_profile` にまとめる：audio-presets の 44100Hz / 256 frames の検証、timeline / engine の frame 契約、rekordbox_timing の補正表と `VERSION`。移植では値を一切変えない。エンジンを置き換えるときにプロファイルを差し替え、VERSION を上げてキャッシュを無効にする（[engine-and-junction.md](engine-and-junction.md)）。

---

## 2. DROP と PORT

### 2.1 DROP

HTTP（呼び出し元なし、またはテストだけ）：`GET /api/`、`POST /api/play/rekordbox/import`、`GET /api/play/rekordbox/{source_id}/tree`、`GET .../playlists/{playlist_id}/tracks`、`POST /api/tracks/{id}/cues-rekordbox`（一括版で代替）、`GET /api/tracks/{id}/similar`（リポジトリ関数は MCP と set planner のために移植）、`GET /api/setlists/{id}/duration`（関数は移植）、`POST /api/wordplay-pairs`（サービスは MCP のために移植）、`GET /api/assist/library-status`（`registered_paths()` は内部用に移植）。

Mirror（常に空。フロントの Mirror UI と同時に削除。D-08）：`GET /api/play/rekordbox/sources`、`.../tree/page`、`.../playlists/{id}/tracks/page`、`POST .../playlists/{id}/copy`。テーブルはファイル互換のため DDL だけ残す。

Tauri へ置き換え：`POST /api/system/save-file`（tauri-plugin-fs）、`POST /api/system/reveal-file`（Tauri コマンド `reveal_in_os`）。フロントの `src/lib/download.ts` と `src/services/system.ts` を同時に変える。

**Explorer（オーナー決定）**：`POST /api/fs/list`、Explorer の Hide Analyzed、Explorer の待ち行列開始処理。代わりの取り込み UX は [README.md](README.md) §2.5。`deviations.yaml` に記録する。

MCP：常に固定エラーを返す Junction の 5 ツール（`junction_request_handoff`、`junction_cancel_handoff`、`junction_accept_handoff`、`junction_attach_live_monitor`、`junction_detach_live_monitor`）。MCP の instructions にある古い Live の記述も直す（D-09）。

中継層と不要コード：Tauri の Junction 中継（TCP・トークン・HTTP パーサ）と env 注入、`backend/mcp_server/junction_bridge.py`、許可リストの `invite.rotate`、誰も送らない入力（`AssistRecommendRequest.genres`、`GridBatchRequest.track_ids`、`PATCH /api/metadata/update` の `lyrics`、トラック検索の `target_params` 経路など）、`--diagnose-*`、不要な env（NUM_WORKERS、MUSIC_DIR、TF_CPP_MIN_LOG_LEVEL、NUMBA_CACHE_DIR、MPLCONFIGDIR）。`operation_journal` への書込み（読み手なし。D-11）と `controller_device_bindings`（DDL だけ残す）。`backend/transfer_db.py` と `scripts/maintenance.py` は `plumdeck-backend` のサブコマンドに吸収する。

### 2.2 新規

`PUT /api/tracks/{track_id}/lyrics`：フロントが呼んでいるのに、バックエンドから消えて 405 になっている。register_lyrics と同じ意味論で復活させる（keywords_* を NULL、updated_at=now、source='user'）。D-02。

### 2.3 PORT（スライスごと）

- **S2**（済）：`GET /`、`GET/POST /api/settings`、`GET /api/dashboard`。
- **S3**：`GET /api/tracks`、`/page`、`/count`、`/ids`、`PATCH /api/tracks/{id}/info`、`/genre`、`GET /api/tracks/{id}/lyrics`、`GET /api/lyrics/search`、`GET /api/genres/{list,subgenres,unknown,unknown-ids,cleanup-suggestions}`、`POST /api/genres/{batch-update,apply-analyses,cleanup-execute}`、`GET /api/stream`、`GET /api/metadata`。
- **S4**：setlists の CRUD、`GET/POST /api/setlists/{id}/tracks`、`/export/m3u8`、`/export/validate`、`GET /api/recommendations/next`、`/next/page`、`POST /api/recommendations/auto`、`/path`、`PATCH /api/setlist-tracks/{id}/wordplay`、wordplay の GET・approve・PATCH・DELETE、`GET /api/tracks/{id}/suggest-genre`、`GET /api/genres/grouped-suggestions`、`/{track_id}`。
- **S5**：`GET/PUT /api/tracks/{id}/performance-metadata`、`POST /api/tracks/performance-metadata/cue-points`、`POST /api/tracks/cues-rekordbox/import`、`POST /api/tracks/{id}/grid-rekordbox`。土台は `plumdeck-rekordbox`。
- **S6**：`/api/assist/` の decks/resolve、recommendations、routes、artists、search、window、agent、agent/list。
- **S7**：`/api/play/` の playlists、sessions、history、recordings、`tracks/{id}/waveform-detail`、`/api/workflows/recordings/` の timeline の GET / PUT、`timeline/engine`、`tracklist.txt`、`external`。
- **S8**：`/api/metadata/` の fetch-lyrics-single、update（POST）、cancel、clear-cache、`PATCH /api/metadata/update`、`POST /api/metadata/fetch-artwork-info`、`POST /api/genres/apply-to-files`、`/api/settings/` の export/csv、import/analyze、import/execute、metadata/export、metadata/import/analyze、metadata/import/execute、WS `/ws/metadata`。
- **S9**：`GET /api/mcp/info` と `/mcp`、MCP の tracks・setlists・genres・lyrics・wordplay・assist・set_planning のツールとプロンプト 2 本。
- **S10**：`/api/workflows/` の summary、media の diagnose / apply / undo、version-groups、setlist-entries の version、backups、restore の inspect / apply、audio-presets、controller-profiles、usb。
- **S11**：`POST /api/ingest`、`GET /api/ingest/status`、`POST /api/ingest/cancel`、grid-jobs、`POST /api/tracks/{id}/grid-analysis`、`/api/workflows/play/imports`、WS `/ws/ingest` と `/ws/play-imports`、MCP の analysis ツール。取り込み UX を作り直す場合は、新しい単一の取り込みキューに合わせる。
- **S12**：MCP の junction ツール（`JunctionPort` 経由でエンジンへ）。

### 2.4 呼び出し元はあるが価値が低いもの（既定は PORT。DROP はオーナー判断）

`GET /api/workflows/summary`（件数フッターのみ）、`POST /api/workflows/media/repairs/{id}/undo`（ほぼ常に conflict）、`POST /api/workflows/usb/handoffs/{id}/duplicate`、`GET .../tracklist.txt`（Tauri の保存ダイアログ候補）、`GET /api/stream`（Tauri の asset プロトコル候補。制限するなら root_path 配下か登録済みのファイル）。

---

## 3. 移植順序

| # | スライス | 依存 | 並行可 | 状態 |
|---|---|---|---|---|
| S0 | 基盤：workspace、compat、契約の抽出、パリティハーネス、ヘッドレス bin | – | – | 済 |
| S1 | DB 層、起動・停止、SQLite キュー | S0 | – | 済（compaction 未移植） |
| S2 | settings、dashboard、`GET /`、ミドルウェア（save / reveal の Tauri 置換は S13） | S1 | S3 | 済 |
| S3 | ライブラリの中核（tracks、lyrics、genres、stream、タグ読み） | S1 | – | 済 |
| S4 | セットリスト、推薦、wordplay、ジャンル提案 | S3 | S5、S8 | |
| S5 | rekordbox、performance metadata、グリッド候補（解析なし） | S3 | S4、S8 | |
| S6 | Assist と set planner | S4, S5 | S7 | |
| S7 | Play、録音、詳細波形、録音 timeline | S4 | S6 | |
| S8 | BackgroundTask、メタデータジョブ、タグ書込み、CSV | S3 | S4、S5 | |
| S9 | MCP（解析と Junction 以外）、`/mcp`、`/api/mcp/info` | S4, S6, S8 | – | |
| S10 | workflows（修復、版、プリセット、USB、バックアップと復元） | S4, S5, S7 | S9 | |
| S11 | 解析に依存する部分（ingest、grid-jobs、選択解析、Play 取り込み、WS 2 本、MCP の analysis） | S5, S7–S10 と Analyzer | – | |
| S12 | Junction の in-process 化（MCP ツール） | S9 | S10、S11 | |
| S13 | Tauri への統合と本番切替 | S2–S12 | – | |
| S14 | Python 撤去、差分台帳の修正、フロントのデッドコード削除 | S13 | – | |

- 各スライスは「Rust のユニットテスト（Python から書き出したベクタ）」「フィクスチャのパリティで説明のつかない差分 0」「自己レビュー」で閉じる。実 DB コピーでのパリティはオーナーのローカル確認に積む。
- S13 までは本番に出ない。開発中は、フロントの `VITE_SERVER_PORT` を Rust のヘッドレス bin に向けて手動確認できる（DB はコピーを使う）。
- **S11 の Analyzer**：(a) Rust の `plumdeck-worker`（[analysis.md](analysis.md)）、または (b) 暫定として既存の Python 解析ワーカーを Rust の `WorkerAnalyzer` から V1 で呼ぶ。(b) なら S13 を Rust 解析の完了から切り離せるが、Python の同梱が解析ワーカーとして残る（オーナー判断）。

---

## 4. パリティ戦略（差分テスト）

### 4.1 構成

```
                 ┌──────── plumdeck-parity (Rust) ─────────┐
 scenarios/*.yaml│ 同じ順に送信 → 正規化 → 比較 → report    │
                 └──────┬─────────────────────────┬────────┘
                        ▼                         ▼
   Python オラクル（tools/parity/oracle_server.py）   Rust ヘッドレス（plumdeck-backend --parity）
   side-A の DB / USER_DATA_DIR / master.db コピー   side-B の同じもの
                        └─ シナリオ終了：両方を graceful 停止（CHECKPOINT）→ db-diff → ファイル diff
```

- `plumdeck-parity` のサブコマンド：`run`（URL 指定で両側に送る）、`suite`（フィクスチャから両方を起動し、シナリオを流し、停止して db-diff）、`startup`（起動・停止の比較）、`db-diff`、`db-stats`。
- `tools/parity/`（移行期間だけの Python 補助）：`oracle_server.py`（オラクル起動。解析・外部 URL・OS コマンド・乱数・時刻を差し替え、実データの場所を指していたら即終了）、`parity_env.py`（安全柵）、`export_vectors.py` / `export_array_vectors.py`（互換ベクタ）、`extract_contracts.py`（schema / OpenAPI / MCP の契約）、`db_stats.py`。
- `contracts/`：`schema/`、`http/openapi.v5.json` と `routes.v5.json`、`mcp/tools.v1.json`・`prompts.v1.json`・`instructions.txt`、`compat-vectors/`、`parity/{scenarios/, normalize.yaml, deviations.yaml}`、`oracle.json`（オラクルの版）。

### 4.2 実 DB のコピー（オーナーのローカルでのみ）

- plumdeck と rekordbox を終了し、開いているプロセスが無いことを確認してから、アプリのデータ（macOS は `~/Library/Application Support/plumdeck/`、Windows は `%LOCALAPPDATA%\plumdeck\plumdeck`）の DB・WAL・ジョブキューを、別の作業ディレクトリへコピーする（APFS なら `cp -c`）。マスタは一度開いて `CHECKPOINT` し、書込み禁止にする。シナリオごとに side-A / side-B を複製する。
- 実音源を書き換える操作（ジャンルのファイル書込み、メタデータ更新、録音の削除、USB の eject、Junction の録音開始）はランナーの denylist で送らない。オラクル側でも例外にし、Rust 側も `--parity-readonly-files` で拒否する。
- 実 DB の応答はリポジトリに入れない。コミットするのは集計だけ。**クラウドのセッションは実 DB モードを使わない。**

### 4.3 コーパス

1. **フロントエンド由来**：`src/services/*.ts` の `apiClient.*` 呼び出しと、直接 URL を組み立てている箇所を列挙し、すべてに 1 つ以上のシナリオを対応させる（CI ゲート）。値は層別に選ぶ（bpm が NULL / 0、key が NULL、NFD のパス、日本語や ß・全角数字の題名、欠損ファイル、埋め込みなし・次元違い、改行だけの歌詞、f32 の境界、overlap 付き setlist、各状態の wordplay、録音、usb_exports）。
2. **OpenAPI 由来**：各エンドポイントの正常系・境界値・型違い（422）。Rust 側に `route_table_matches_openapi`（登録ルートが「OpenAPI − DROP + 追加」と一致）を置く。
3. **MCP スキーマ由来**：必須だけ・全引数・enum / min / max / pattern / maxLength の境界・型違い・JSON 文字列で包んだ引数・数値文字列・未知の引数。`initialize`、`tools/list`、`prompts/list`、`prompts/get` も比較する（protocolVersion `2025-03-26` の legacy session）。
4. **既存 pytest 由来**：TestClient と MCP の call_tool をラップして記録し、Rust に再生する。純関数のテストは JSONL のベクタにして Rust のユニットテストの入力にする。
5. **実トラフィックのキャプチャ**（オーナーのローカル）：`plumdeck-parity proxy` を挟んで普段どおり操作し、両側に再生する。
6. **Assist の再生**：履歴の遷移に対して全プリセット × transition × tempo_tolerance × フィルタで推薦を作り、順位付きの ID 列、summary、reasons、notes を完全一致で比較する（時刻固定）。
7. **検索のファジング**：題名・artist のトークン、部分文字列、NFD、`%` と `_`、日本語、ß、全角数字。

### 4.4 シナリオの形式

`name, db: fixture, fixture, freeze_now?, steps[], compare_overrides, side_effects[]`。steps は `http`、`mcp`、`ws`、`wait{job}`。応答の値は `$prev.body.id` のように後続で使える。意図的な差分のステップには `deviation: {id, b_status[, b_body]}` を付ける（台帳にある ID のみ）。シナリオ終了後に全テーブルを PK 順、`duckdb_sequences()`、SQLite キューまで比較する。

### 4.5 正規化（`contracts/parity/normalize.yaml`）

| # | 対象 | ルール |
|---|---|---|
| N1 | JSON | パースして比較。キー順は無視、キー集合は一致を要求 |
| N2 | 数値 | 整数と保存 DOUBLE は完全一致。f32 由来は f32 で比較。Python 側計算のスコアは相対 1e-9、DuckDB の cosine を経由する値は 1e-6 |
| N3 | 時刻 | naive ISO を両側に要求。シナリオ中に作られた行はローカル壁時計の範囲内を確認してから置換（UTC ずれの検出）。`freeze_now` では完全一致 |
| N4 | sequence ID | 完全一致 |
| N5 | 生成 ID | 側ごとに初出順で α-renaming し、相互参照の整合も検査 |
| N6 | 順序未定義 | `unordered.yaml` のものは id でソートして比較（Rust は ORDER BY id を付ける。D-01） |
| N7 | エラー | status は一致。業務エラーの detail は完全一致。pydantic の 422 は「422 で detail が配列」まで |
| N8 | テキスト応答 | m3u8、CSV、tracklist.txt はバイト一致。Content-Type と Content-Disposition も比較 |
| N9 | バイナリ / Range | status、Content-Range、Content-Length、Accept-Ranges、Cache-Control、本文の sha256 |
| N10 | WS | 初回、`processing` への移行、終端を比較。revision の単調性を検査 |
| N11 | MCP | tools/list は名前・description・instructions・inputSchema（title を除き nullable の書き方を同一視）。結果は text をパースしたものと structuredContent、isError |
| N12 | Python の 500 | 比較対象外（Rust の応答を記録）。Rust の 500 は、Python が 500 以外なら失敗 |
| N13 | DB 状態 | 全テーブルを PK 順。時刻は N3、uuid は N5。fingerprint は完全一致 |
| N14 | ヘッダ | JSON の content-length と date は無視。CORS はプリフライトのシナリオだけ |

### 4.6 副作用

- ファイル：sandbox の USER_DATA_DIR を走査して比較。USB の XML はバイト一致、`manifest.json` は created_at などを除いて一致、backup の zip はエントリと sha256、中の DB は db-diff。
- タグ書込み：フィクスチャの音源を側ごとに複製して書かせ、mutagen と lofty の両方で読み戻して比較（版やフレーム構成の差は D-07）。
- 外部 HTTP（iTunes / LRCLIB）はスタブが受けたリクエスト列、OS コマンドはフェイクの記録、Junction は (action, args) の列と redact 済みの結果を比較する。

### 4.7 Python 撤去後

`plumdeck-parity fixture build` が SQL からフィクスチャ DB を決定的に作る（Python 不要）。小さな合成音源は fixtures にコミットする。S13 までに、フィクスチャのスイートの Python 応答をゴールデンとして記録し、S14 以降は Rust とゴールデンを比較する。

### 4.8 合格基準

- スライスごと：担当範囲のエンドポイントとツールに正常・境界・異常のシナリオ、フロントの呼び出し箇所の網羅 100%、フィクスチャで説明のつかない差分 0、更新系の後の db-diff 0、p95 が Python の 1.2 倍以内。実 DB での差分 0 はオーナー確認。
- S13：`scripts/smoke-backend.py` がヘッドレス bin と同梱アプリの両方、macOS と Windows で通る。Assist の再生で順位列が完全一致。手動チェックリスト。初回起動の前に `.pre-rust-<ts>` のバックアップが作られる。

---

## 5. 差分台帳（`contracts/parity/deviations.yaml`）

| ID | 内容 | 状態 |
|---|---|---|
| D-01 | genres/unknown と unknown-ids に ORDER BY id | 実装済み（proposed） |
| D-02 | `PUT /api/tracks/{id}/lyrics` を復活 | 実装済み（proposed） |
| D-03 | MCP で ToolError 以外の例外の理由も表示（Junction の秘密は出さない） | 案（オーナー判断） |
| D-04 | 録音の書き出しは WAV / FLAC のみ | 案（オーナー判断） |
| D-05 | 詳細波形はデコーダが変わるので許容誤差で比較し、キャッシュを v2 に | 案 |
| D-06 | ループバック以外の Host / Origin を拒否（421 / 403） | 実装済み（proposed） |
| D-07 | タグ書込みを lofty の出力に（rekordbox で読めるか実機確認） | 案 |
| D-08 | Mirror の 4 ルートと UI を削除 | 案（オーナー判断） |
| D-09 | Junction の廃止 5 ツールを削除 | 案（オーナー判断） |
| D-10 | backup の排他は最大 2 秒待ってから拒否、長時間の作業中は即拒否 | 実装済み（approved） |
| D-11 | operation_journal に書き込まない | 案 |
| D-12 | 一致しないリクエストに FastAPI 形式の JSON 404 / 405 | 実装済み（proposed） |
| D-13 | FastAPI 組み込みの文書ルート（/openapi.json、/docs、/redoc）を移植しない | proposed |
| D-14 | 新しいアプリが書いたスキーマ版の DB を、触る前に拒否 | 実装済み（approved） |
| D-15 | Explorer の `/api/fs/list` と待ち方を移植しない | オーナー決定済み（proposed で記録） |
| D-16 | `/api/stream` の Content-Type を固定表に（Python 3.12 の既定 ＋ macOS の Apache mime.types の音声型） | 実装済み（proposed） |

「案」は計画上の案で、まだ yaml には載っていません。該当するスライスで実装するときに `proposed` として追加してください。

台帳に無い差分は入れない。新しい差分は、ID、出典、要約、現状（Rust がどう振る舞うか、どのシナリオで固定しているか）を書いて `proposed` で追加する。

---

## 6. 主要リスク

| # | リスク | 対処 |
|---|---|---|
| R1 | 一括切替になる | S0〜S12 をパリティで閉じる。S13 で初回起動前に自動バックアップ。1 リリースは Python フォールバック。新しい版の DB を開かないガード（D-14） |
| R2 | ストレージと WAL の互換 | duckdb 1.5.5 に固定。実 DB のコピーと WAL を開いて行数を照合（オーナー）。2.x への更新は別のマイグレーション |
| R3 | タイムゾーン（ICU が無いと CURRENT_TIMESTAMP が UTC） | SQL に `now()` も `CURRENT_TIMESTAMP` も書かない（grep で禁止するテスト）。`Clock` のローカル naive 時刻を bind。N3 で検出 |
| R4 | in-process での障害の波及 | panic は CatchPanic で 500。デコードと解析はワーカーのサブプロセス。`--backend-server` の隔離モードを予備に |
| R5 | ランタイムの詰まり（同期 I/O、SQLCipher の PBKDF2、全曲 sha256） | `spawn_blocking` と pool 上限。master.db の接続を stamp 付きで再利用。(path,size,mtime_ns) の sha キャッシュ（size か mtime が変われば無効） |
| R6 | 並行性の意味論 | ゲート 1 本と write_lock 1 本。既存の並行性テストを Rust に移植 |
| R7 | バイト互換が要るもの（USB の snapshot_hash、wordplay_json、CSV、M3U8、XML、fingerprint、:g 書式） | compat を 1 か所にまとめ、大量のベクタで検証。snapshot_hash を再現できなければ serializer_version=2（オーナー判断） |
| R8 | ORDER BY の無いクエリの非決定性 | N6 で吸収。Rust は ORDER BY id。Assist の再生で順位への影響を測る |
| R9 | MCP のワイヤ互換 | `contracts/mcp/tools.v1.json` への契約テスト、lax デシリアライザ、allowed_origins の明示、smoke-backend.py を CI で |
| R10 | SQLCipher のビルドと鍵 | Windows は vendored-openssl。鍵は導出と env `PLUMDECK_REKORDBOX_KEY` での上書き。無いときは 503 か 200 + 警告（performance-metadata の GET は失敗させない） |
| R11 | LIST パラメータ（`IN :tuple`） | 整数 ID は `string_split`、テキストはプレースホルダ展開。ページングとスコアの一致テストを移植 |
| R12 | 解析への依存 | Analyzer トレイトと FixtureAnalyzer でパリティ。タグ読み（tinytag と lofty）の差で取り込みのスキップ判定が変わることも見る |
| R13 | エンジン依存の仕様 | `engine_profile` に隔離し、値を変えない |
| R14 | ユーザーデータのパス（Windows の platformdirs） | 自前で組み立て、Windows CI で検証 |
| R15 | セキュリティ（任意ファイルの読み書き、/mcp に認証なし） | Host / Origin の検証、save / reveal を Tauri に、録音の unlink を録音ディレクトリ内に限る（S14） |
| R16 | データ消失（in-process の restore、rollback、録音の削除） | DbHandle を差し替えて開き直す API（S1 済）、restore のクラッシュ回復テスト、`.bak` の保持方針 |
| R17 | パッケージングと CI | sccache。release.yml と windows-distribution.yml の smoke を Rust 版に |
| R18 | スコープの膨張 | バグ互換を原則にし、修正は台帳を通して S14 で |
