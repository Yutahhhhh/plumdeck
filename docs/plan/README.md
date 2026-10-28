# plumdeck 完全 Rust 移行 計画（引き継ぎの入口）

この `docs/plan/` は、plumdeck の完全 Rust 移行と rekordbox 同等化を、どの環境のセッションでも続けられるようにするための引き継ぎ資料です。最初にこのファイルを読み、担当する作業の詳細を各ファイルで確認してください。

| ファイル | 内容 |
|---|---|
| [README.md](README.md) | 目的、オーナー決定（拘束力あり）、現状、作業キュー、作業ルール、オーナー確認リスト |
| [backend-port.md](backend-port.md) | Python バックエンド → Rust（in-process）のポート計画。スライス S0〜S14、crate 構成、パリティ戦略、差分台帳、リスク |
| [rekordbox-parity.md](rekordbox-parity.md) | rekordbox 同等化。GAP マトリクス、データモデル、自前検出、評価、作業項目 RB-xx |
| [engine-and-junction.md](engine-and-junction.md) | Mixxx エンジン → 自作 Rust エンジン、C++ Junction → Rust。BlackHole 適合スイートを受け入れゲートにする |
| [analysis.md](analysis.md) | 最小の解析仕様（lean spec）と残りのフェーズ |

文書の言語は日本語を基本にします（オーナーの言語）。コード、識別子、コミットメッセージは英語です。

---

## 1. ゴール

**plumdeck を、ライセンス上も実装上も自分たちのものにし、rekordbox が無くても rekordbox 同等に使える DJ アプリにする。**

1. **バックエンドの完全 Rust 化**：Python（FastAPI + DuckDB + MCP + 解析）サイドカーを廃止し、Rust バックエンド（axum + rmcp + duckdb-rs）を Tauri プロセス内で動かす。ポート・パス・応答・DB ファイルは互換のまま（[backend-port.md](backend-port.md)）。
2. **エンジンの完全自作**：Mixxx ベースのエンジンホストを、自作の Rust エンジン（`plumdeck-engine`）に置き換える（[engine-and-junction.md](engine-and-junction.md)）。
3. **Junction の Rust 化**：C++ の Junction（B2B 引き継ぎ）を Rust に移植し、エンジンに統合する。Junction は一度も無効化しない。
4. **rekordbox はオプション**：rekordbox があればそのデータ（BPM、PQTZ グリッド、キー、キュー、PWV6/PWV7 の 3 バンド波形、フレーズ）を読む。無い利用者には、自前解析で rekordbox 同等の品質と、rekordbox に近い UI（フレーズ表示、3 バンド波形、キーの相性表示）を提供する（[rekordbox-parity.md](rekordbox-parity.md)）。
5. **解析は最小限（lean）**：今使っている機能に要る出力だけを計算する。前景は「タグ → rekordbox 照合 → 1 回のデコード」だけにし、埋め込みはバックグラウンド（[analysis.md](analysis.md)）。
6. 結果として、インストーラを大幅に小さくし、出荷物から GPL / AGPL / LGPL / CC-NC をなくす。

---

## 2. オーナー決定（拘束力あり。再確認せずに従う）

### 2.1 ライセンスと所有

- **D1 所有ライン**：許諾型ライセンスと MPL-2.0 の基盤ライブラリは使ってよい（cpal、symphonia、libopus、tract、lofty、duckdb、rusqlite など）。**DJ/DSP のロジックはすべて自作**する（keylock、EQ、FX、スクラッチ、シンク、解析）。出荷物から GPL / AGPL / LGPL / CC-NC を完全になくす。GPL のコードを比較用に**テストでだけ**動かすのはよいが、出荷しない。
- **クリーンルーム**：GPL のソース（Mixxx の effects/ や rekordbox 取り込み、rbox など）は読まない。読んでよいのは公開仕様、公開の形式資料、オーナー自身が書いたコード。詳細は各計画ファイルのクリーンルーム規則に従う。
- **学習済みモデル**：各利用者の PC 上で、その利用者自身の検証済みラベルから学習する。**オーナーの商用音源で学習した重みは出荷しない。**
- **MusiCNN（D3）**：ISC ライセンスの MSD_musicnn チェックポイントを自分で ONNX に変換し、Rust（tract）で推論する。埋め込みは任意（「解析済み」の条件にしない）。数値が旧版と一致しなければ全曲を再計算する。
- 公開リポジトリなので、UI に rekordbox / Pioneer のロゴ、アイコン、画像、商標を使わない。

### 2.2 移行の範囲と順序

- **完全移行を承認済み**：Python バックエンド、Mixxx エンジン、C++ Junction をすべて Rust に置き換える。完了したらレビューを受ける。
- **D2**：Rust エンジンと Rust 版 Junction は、既存のエンジン契約の裏で並行開発し、パリティを確認してから一度に切り替える。それまで Mixxx は出荷物に残す。
- 移行は一括切替（S13）。DuckDB は 1 ファイルを書込みで開けるのが 1 プロセスだけなので、Python と Rust を同じ DB で並走させられない。

### 2.3 rekordbox

- **rekordbox はオプション**。他の利用者は rekordbox を持っていないかもしれない。すべての機能が rekordbox 無しで動くこと。
- **優先順位は「手動 ＞ rekordbox ＞ 自前」**。曲 × 機能の単位で解決する。自前解析は rekordbox データが無いときに遅延で走ってよい。
- **rekordbox へは書き戻さない（読み取り専用）**。
- BPM・グリッド・キーは rekordbox を出どころにしたい。ただし**キーの出どころを rekordbox に切り替えるのは、DB コピーで `assist_replay_eval` を回して劣化が無いと確認してから**（今は既定 OFF）。rekordbox にキーが無い曲は自前検出を残す。
- rekordbox 機能の網羅を目指す：フレーズ解析と表示、キー表示、2 曲のキーの相性表示を含む。
- 自前の BPM / グリッド / キー / 波形の品質評価には、オーナーの rekordbox データを参照セットとして使う（ローカルでのみ。データはリポジトリに入れない）。

### 2.4 ジャンルと AI

- **rekordbox のジャンルやファイルタグのジャンルは使わない**（不正確で、使い回されたタグにすぎないため）。
- ジャンルは、plumdeck で人が検証したジャンル（`is_genre_verified=TRUE`）で**端末上で学習する自前の分類器**で付ける。
- 任意の AI（MCP、または利用者自身の API キー）は**確信度の低い曲だけ**を扱い、タグのジャンルをヒントとして渡さない。最終決定は常に人の検証。
- **アプリ内 AI は任意で許可**：MCP に加えて、利用者が Claude / OpenAI / Gemini の API キーを入力して使える機能を置いてよい（ジャンル分類、自然言語の機能など）。キーも AI も任意で、中核機能の必須条件にしない。キーは OS のキーチェーンなど安全な場所に保存し、起動時に削除される旧 LLM 設定キーとは別の名前にする。
- Assist の「XX のような曲」は UI で数値的に決めない。利用者自身の AI エージェントが MCP 経由で扱う（数値の類似度は MCP の根拠としてだけ出す）。
- セット全体の計画（rekordbox プレイリストのフェーズ構成、アンケートからのセット作り）は **MCP 専用**。アプリ UI は作らない。

### 2.5 取り込み UX

- **Explorer UI を廃止**する（`src/components/file-explorer`、`/api/fs/list`、Hide Analyzed、Explorer の待ち行列開始処理）。**Rust に移植しない。**
- 取り込みは次の 3 経路にし、**1 つの取り込みジョブキュー**で処理する。
  1. アプリ全体へのファイル / フォルダのドラッグ＆ドロップ
  2. Dashboard と Library の「曲を追加」ボタン（OS のファイル / フォルダ選択を開く）
  3. Dashboard の「rekordbox コレクションにあって plumdeck に無い曲」の取り込み（rekordbox があるときだけ表示）
- Dashboard に **「要対応」** セクションを置く。未解析・失敗・音色が既定値・埋め込み待ち・古い解析の件数と、ワンクリックの解析 / 再試行。

### 2.6 キー表示と推薦

- **キー表記の既定は Alphanumeric（8A 形式）**。Classic（Am など）に切り替えられる。UI のラベルは「Camelot」ではなく **「Alphanumeric」**（商標の主張があるため）。
- **Traffic Light（相性の良いキーの色分け）は既定 ON**。推薦フィルタとは独立。
- **キー相性の推薦フィルタ（Camelot ON/OFF）は既定 OFF**。
  - OFF：キーを無視する（今の Assist と同じ）。
  - ON：同じキー、同じ文字で番号 ±1、同じ番号で文字違い、の候補だけにし、同じキーを先頭に、**キー不明の曲は最後**に並べる。
  - 対象：Assist、Play の推薦パネル、Setlist の Recommend / Auto、MCP の `recommend_next_track`。選択は保存する。
- 推薦のスコアでキーの重みは 0 のまま。埋め込みが無い曲は 0 点にせず、残りの重みで正規化し直す。
- 根拠（オーナーの実際の DJ 履歴から定性的に分かったこと）：テンポはかなり厳密に合わせるが、キーは合わせていない（相性の良いキーへの移行は、テンポを合わせたランダムな組と同程度）。同じジャンル・サブジャンルへの移行はランダムよりずっと多い。**再生履歴が最も強い信号**。Assist の重みを変えるときは、履歴を時間順に再生する評価（後のセットが前のケースに漏れないこと）で確かめてから信じる。

### 2.7 Play モード

- **rekordbox 同等の挙動**にする：機能、挙動、配置、色の意味を合わせる。アセット（ロゴ、アイコン、画像、色の値）は自前。ラベルは一般的な DJ 用語だけにし、メーカー固有の名前は使わない。
- **キーボードの既定は「Performance 1」相当の配列**。今の配列は「plumdeck クラシック」として選べる。
- **PAD FX の既定バンク**（全デッキで 2 バンクを共有）：
  - Bank 1（パッド 1〜8）：ROLL 1/2、SWEEP 80、FLANGER 16、V.BRAKE（release）3/4、ECHO 1/4、ECHO 1/2、REVERB 50、ECHO（release）1/2
  - Bank 2：SLIP LOOP 1/16、1/8、1/4、1/2、REV ROLL 1/16、1/8、1/4、1/2（param 50）
  - 2 つのビューを作る：通常の「使用中」ビューと、歯車で切り替えるインライン編集ビュー（パッドごとに FX の種類・拍・パラメータのドロップダウン）。
- 汎用の FX1/FX2 パネルは廃止し、FX は BEAT FX に一本化する。
- MEMORY CUE は DB に移し、rekordbox から取り込めるようにする。
- 新しい Play の操作は、作り直す前にすべての操作が実際に動くか監査する。「押しても何も起きない操作」をゼロにする。

### 2.8 UI の品質

- 利用者向けの画面は、**洗練された音楽アプリに見えること。デバッグ画面やシステムの進捗表示のように見せない。**
  - 状態は親しみやすい 1 行と、なめらかな進捗表示 1 つで示す。
  - 技術的な詳細（ステージ ID、生のエラー、カウンタ、タイマ）は、エラーや遅延のときだけ出る折りたたみの「詳細」に入れる。
  - エラーは人向けの文言と、はっきりした再試行ボタンを先に出す。
  - 新しい UI（オンボーディング、取り込み、Dashboard の要対応）にも適用する。

### 2.9 範囲外

- **当面範囲外（決定）**：ライティング、ビデオ、DVS、ストリーミング、クラウド連携、CDJ 用 USB（export.pdb）の作成、rekordbox への書き戻し。
- **STEMS は後で、代替手段で**：許諾型ライセンスのローカル分離モデル、利用者の API キーで使うクラウドの stem API、DSP による疑似 stem のいずれかを調査する。
- 範囲外の提案（オーナー未決。勝手に着手しない）：PRO DJ LINK、Tag List、CoBeat、SYNC MANAGER、モバイル連携、Collaborative Playlist、Mixcloud / YouTube 共有、サンプルパック、GROOVE CIRCUIT、EDIT モード、Ableton Link、Pad Editor、DUAL PLAYER、Radar 互換。

### 2.10 Junction（B2B）の要件

- 本物の B2B と同じ：操作する DJ は常に 1 人だが、出ていく DJ の音は、入ってくる DJ の「JUNCTION デッキ」（レベル、EQ、クロスフェーダー、CUE を持つデッキ枠）で鳴り続け、入ってくる DJ がフェードアウトしたときに初めて止まる。**操作者の交代と音の停止は別の手順。**
- Mac → スマホ方向は同期のずれを許容する（スマホにはクロスフェーダーとレベルフェーダーだけでよい）。
- UI は「現在 / 次」の別カードではなく JUNCTION レーン 1 本にし、グリッド範囲の色分けでどちらの曲かを示す。

---

## 3. 現状（どこまで終わっているか）

### 3.1 `feat/release-integration`（このリリースで出すもの。リモートにある）

`origin/main` からの差分は次のとおり。

- **解析の整理（[analysis.md](analysis.md) の A1〜A4）**：読まれない出力の計算停止、`features_extra` の許可リストと再構築、BPM / キーを「手動 ＞ rekordbox ＞ 自前」で採用（キーの rekordbox 採用は既定 OFF、OFF に戻すと自前のキーを復元）、rekordbox コレクションからの既存曲の BPM / キー同期、PQTZ がある曲はグリッドの一括再解析から外してその場解析、埋め込みの任意化とバックグラウンド補完（起動時のバックフィルと件数表示）、旧来の推薦器を Assist と同じ採点に揃える（キー重み 0、埋め込み欠損は再正規化）、CSV 取り込みの BPM / キーを手動値として記録、同じ音源と証明できたときだけ派生データを保持。
- **Play モードの改善**：エンジン契約（protocol.ts）の拡張、BEAT FX への一本化、PAD FX のバンクとスロット、決定済みの既定 PAD FX（パッド 1〜8）、Performance 1 のキーボード、ミキサー（COLOR FX、クロスフェーダーのアサインとカーブ、EQ キル、メーター、ドロップアウト）、ピッチベンドとテンポ、KEY SHIFT / SLIP / REVERSE、MEMORY CUE パッド（保存先はまだブラウザ側）、MASTER と BEAT SYNC の修正、Mixxx ホスト側の H-1〜H-12 相当（メーター、sync テレメトリ、KEY SYNC、SLIP 復帰、ヘッドホン、ループ範囲、クロスフェーダーカーブ、PAD FX / BEAT FX の底上げ）、同期開始の位相シーク。
- **BlackHole 適合スイート**：`native/mixxx-engine-host/tests/conformance/`（`PLUMDECK_TEST_HOST` で有効化、BlackHole 2ch が適用されていないと音を出さない）。QB-1〜QB-9 と H-x 項目を測る。CI では解析ヘルパーの自己テストだけを回す。
- **起動画面の作り直し**（ブランドの落ち着いたスプラッシュ、詳細は折りたたみ）。
- Windows のマクロ衝突の修正、macOS のローカル署名で権限が消える問題の修正など。

### 3.2 `feat/rust-migration`（この資料があるブランチ）

分岐点は `feat/release-integration` の初期（解析整理と Play 改善の**前**）。段 0 で `origin/feat/release-integration` をマージ済み（`contracts/` 再生成、S0〜S2 のパリティを再確認）。

| 項目 | 状態 | 主な場所 |
|---|---|---|
| S0 基盤 | 完了 | `src-tauri/Cargo.toml`（workspace）、`src-tauri/crates/plumdeck-core`（Python 互換部品）、`contracts/`（schema / http / mcp / compat-vectors）、`tools/parity/`（Python オラクルと補助）、`src-tauri/crates/tools/plumdeck-parity`、ヘッドレス bin `plumdeck-backend` |
| S1 DB 層と起動・停止 | 完了（起動時の compaction は判定と `features_extra_clean` の印まで。再構築は未移植。解析ジョブキューは起動時に開いて回復するが、待機中ジョブの起動と埋め込みのバックフィルは S11） | `src-tauri/crates/plumdeck-db`（DuckDB 1.5.5 固定、ハンドルとゲート、スキーマ初期化、新しい版の DB の拒否、restore の回復、SQLite ジョブキュー、BLOB コーデック）、`plumdeck-backend/src/startup.rs` |
| S2 settings / dashboard / ミドルウェア | 完了（save / reveal の Tauri 置換は S13 で） | `plumdeck-backend/src/routes`、`middleware.rs`、`contracts/parity/scenarios/s2-*.yaml` |
| S3 ライブラリの中核 | 完了（tracks / page / count / ids、info / genre の編集、歌詞の取得と検索、`PUT /api/tracks/{id}/lyrics` の復活（D-02）、genres の一覧・未判定・表記揺れ・一括更新・解析結果の適用、`/api/stream`、`/api/metadata`。`/api/fs/list` は移植しない（D-15）） | `plumdeck-domain`（キー表記、ジャンル規則、LRC）、`plumdeck-media`（lofty のタグ読み、Content-Type 表、NFC/NFD 解決）、`plumdeck-db/src/repos/{tracks,lyrics,genres}.rs`、`plumdeck-backend/src/{lax,query,models}.rs` と `routes/{tracks,lyrics,genres,media}.rs`、`contracts/parity/scenarios/s3-*.yaml` |
| `plumdeck-rekordbox` | RB-10（master.db）と RB-11（ANLZ）完了 | `src-tauri/crates/plumdeck-rekordbox`（DuckDB 非依存） |
| `plumdeck-decode` | W1-04 / W1-05 相当完了（HE-AAC と Opus は型付きの Unsupported） | `src-tauri/crates/plumdeck-decode`（`spec.rs` にクリーンルーム仕様） |
| 差分台帳 | D-01、D-02、D-06、D-10、D-12〜D-16 を記録済み | `contracts/parity/deviations.yaml` |
| CI | `pr-checks.yml` の `backend-port` ジョブ（Linux）、Windows で `plumdeck-core` | `.github/workflows/pr-checks.yml` |

### 3.3 未着手（主なもの）

- バックエンド：S4〜S14 すべて。
- rekordbox 同等化：RB-01 の残り（HOT CUE マーカーの色の食い違いは要確認。Setlist 自動生成のキー変換の部分一致バグは修正済み）、RB-03 以降すべて（キー表記の設定、Traffic Light、推薦のキー相性フィルタ、フレーズ、PDW3 波形、MEMORY CUE の DB 化など）。
- 解析：A5（Windows の整理。Rust 移行に畳み込む）、A6（Rust の MusiCNN）、A7（Rust 解析ワーカー、Essentia 撤去）、自前検出 v1（RB-30〜RB-36）。
- エンジンと Junction：W2-01 以降すべて、切替 W3-01〜W3-04。
- 取り込み UX：Explorer 撤去、ドラッグ＆ドロップ、「曲を追加」、Dashboard の要対応と rekordbox コレクション取り込み。
- ジャンル分類器、アプリ内 AI キー、STEMS の調査。
- Play の残り：波形の統一（3 バンド、共通ズーム、キュー色の共通化）、ブラウザ（Traffic Light、ΔBPM、MASTER 基準のフィルタ、再生済みの色、キー相性フィルタ）、PAD FX のインライン編集ビュー、Rust エンジンでしか作れない項目（[engine-and-junction.md](engine-and-junction.md) §4）。

---

## 4. 作業キュー（順序と依存）

上から順に進めます。同じ段の項目は依存が無いので、どれからでもよいです。「（Mac）」「（Win）」は実機が要る確認で、クラウドのセッションは実装と自動テストまで行い、確認は §6 のオーナー確認リストに積みます。

### 段 0：ブランチの同期（最初に 1 回）

0. `feat/rust-migration` に `origin/feat/release-integration`（main に入っていれば `origin/main`）をマージする。Python の挙動が変わっているので、続けて次を行う。
   - `tools/parity/extract_contracts.py` と `tools/parity/export_vectors.py` で `contracts/` を再生成し、差分をコミットする。
   - S0〜S2 のパリティスイートと `cargo test` を通し直す。
   - `pr-checks.yml` と `src-tauri/Cargo.toml` / `Cargo.lock` の衝突に注意する。

### 段 1：DuckDB に依存しない基盤（並行可）

- ~~**RB-02** `plumdeck-key`~~（完了。`src-tauri/crates/plumdeck-key`、`contracts/key/keys.json`、`src/lib/keys.generated.ts`。`normalize_key` は Python の全件ベクタ `contracts/key/normalize_key.jsonl` で一致を確認）。
- ~~**S3** ライブラリの中核~~（完了。`/api/fs/list` は D-15 として DROP を記録）。
- **W1-06 / Opus**：`plumdeck-decode` の OS フォールバック（HE-AAC）と Opus（Mac / Win のランナーが要る）。
- ~~**W1-07** `plumdeck-dsp`~~（完了。ポリフェーズと可変比のリサンプラ、Hann / Kaiser、STFT、RBJ biquad、RMS / peak。通過域 ±0.0001 dB 未満、阻止域 −93 dB 以下、処理中の割り当て 0 をテストで確認）。
- **W2-01** エンジン契約 v1 の機械可読化（op・イベント・capability・エラー・定数とドリフト検査は済。params の Schema は W2-09 で）。
- ~~**W2-30** Junction の仕様ベースライン~~（完了。`native/junction-spec/`：テスト 213 件、コマンド 51、制御メッセージ 9、定数 37。`tools/junction-spec/baseline.mjs` の検査を CI で実行）。

### 段 2：バックエンドの主要スライス

- **S4**（実装済み。セットリストはパリティ 0 差分。推薦・wordplay・ジャンル提案のシナリオは最後にまとめて追加。D-17 / D-18 を記録）／ **S5**（rekordbox crate を使う performance metadata とグリッド候補）／ **S8**（BackgroundTask、メタデータジョブ、タグ書込み、CSV）。依存はすべて S3。
- **S6**（Assist と set planner。S4・S5 の後）／ **S7**（Play、録音、詳細波形。S4 の後）。
- **S9**（MCP。S4・S6・S8 の後）→ **S10**（workflows。S4・S5・S7 の後）→ **S12**（Junction の MCP ツールを in-process に。S9 の後）。

### 段 3：解析の Rust 化（S11 の前提）

- **W1-02 / W1-03**：時間軸の仕様と計測（Mixxx の計測は Mac / Win のランナーか実機）。
- **RB-30** 評価ハーネス（参照データの抽出はオーナーのローカルでのみ）。
- **RB-31〜RB-34**（波形、テンポ・拍・小節頭、キー、フレーズ）、**W1-24**（音色 4 種）、**W1-27 / W1-28**（MusiCNN の ONNX 化と tract）。
- **W1-08** `plumdeck-worker`（V1 プロトコル）。
- **S11**（ingest、grid-jobs、選択解析、Play 取り込み、WS 2 本、MCP の analysis）。Analyzer は Rust ワーカーが合格していればそれを、まだなら暫定で既存の Python 解析ワーカーを V1 で呼ぶ。

### 段 4：切替とその後

- **S13** Tauri への統合と本番切替（Mac / Win の smoke、Assist の再生一致、手動チェックリスト、初回起動前の自動バックアップ）。
- **S14** Python 撤去、台帳の差分の修正、フロントのデッドコード削除。
- **RB-13**（3 層のデータモデルと解決器）、**RB-40**（自前パイプラインを既定に）、取り込み UX、キー表示系（RB-03〜RB-06）、フレーズ表示（RB-21）、PDW3（RB-20）。

### 段 5：エンジンと Junction（段 1 以降と並行）

- エンジン：W2-02（BlackHole の CI スパイク）→ W2-03 / W2-04 → W2-05〜W2-24 → W2-25（プレビュー版）→ W2-26（パリティ・キャンペーン）。
- Junction：W2-31〜W2-35 → W2-36（トランスポートの go/no-go）→ W2-37〜W2-44 → W2-45 / W2-46 → W2-47。
- 切替：W3-01（Rust のみ）→ W3-02（Mixxx と C++ Junction のソース削除）→ W3-03（timeline-v2）→ W3-04（Junction W6 既定化）。
- 受け入れゲートは BlackHole 適合スイート（Mac）とオーナーの試聴・実機確認。

### 新機能を入れる場所の方針

- フロントだけで完結するもの、DuckDB に依存しない Rust（`plumdeck-key`、rekordbox の読み取りを Tauri コマンドで出す RB-14）は、ポートを待たずに入れてよい。
- バックエンドの API や DB を変える新機能は、原則として**該当スライスを Rust に移植した後に Rust で実装**し、`deviations.yaml` に追加（addition）として記録する。Python 側に先に入れる場合は、そのスライスを移植する前にパリティのコーパスへ追加する。
- オーナーに見えている P0 のバグ修正だけは、どちらの側でもすぐ直してよい（両方に入れ、台帳とコーパスを更新する）。

---

## 5. クラウドセッションの作業ルール

### 5.1 環境

- Linux、オーナーの Mac にはアクセスできない前提。オーナーの音源・DB・rekordbox データは**クラウドに一切置かない**。
- Rust は stable（`rust-version = "1.91"` 以上）。DuckDB と SQLCipher は C/C++ からビルドされるので、初回ビルドは重い。
- パリティのオラクルには Python 3.12 の venv が要る（`backend/requirements-pr.txt` を基準に、`contracts/oracle.json` に記録された版に合わせる。パッチ版まで合わせること：3.12.3 と 3.12.13 では float の divmod のベクタが変わる。`uv python install 3.12.13` で入る）。`PLUMDECK_ORACLE_PYTHON` でその python を指定する。`backend/main.py` が aiohttp を import するので venv に入れる。互換ベクタ（`contracts/compat-vectors`）は darwin（arm64）で作ったもので、x86 で作り直すと NaN の符号だけが変わる。作り直さない。

### 5.2 ブランチと PR

- 1 スライス（または 1 作業項目）＝ 1 PR。`feat/rust-migration` から `port/S3-library` のようなブランチを切って PR を `feat/rust-migration` 向けに出すか、`feat/rust-migration` 上でそのまま続ける。
- main への push やマージはしない。マージはオーナーが行う。
- コミットは小さく、メッセージは英語で「何を・なぜ」。末尾に規約どおりの署名行を付ける。

### 5.3 ビルドとテスト

- **DuckDB を使う crate 群は必ず一緒にビルド・テストする**：

  ```sh
  cd src-tauri
  cargo test -p plumdeck-core -p plumdeck-db -p plumdeck-domain -p plumdeck-media -p plumdeck-key -p plumdeck-backend -p plumdeck-parity
  ```

  別々に回すと build-dependency の feature が分かれ、bundled DuckDB を何度もビルドし直すことになる。新しく DuckDB を使う crate を足したら、このリストと CI の `backend-port` ジョブに加える。
- DuckDB 非依存の crate は単独でよい：`cargo test -p plumdeck-rekordbox`、`cargo test -p plumdeck-decode`、`cargo test -p plumdeck-dsp --release`（品質テストは release で回す）。
- `#[ignore]` の「実ライブラリ」テストは、オーナーのローカルデータを前提にしているので、クラウドでは回さない。
- Tauri 本体の `cargo test --lib` と、フロントの node テストも壊さない。

### 5.4 パリティ

- **既定はバグ互換**。応答と DB 書込みを Python と一致させる。意図的な差分は `contracts/parity/deviations.yaml` に、`status: proposed` で理由と現状を書いてから入れる（承認はオーナー）。既知のバグは S14 まで直さない。
- **パリティは Python オラクルとフィクスチャ DB だけで行う**（`plumdeck-parity suite --work <dir> contracts/parity/scenarios/<file>.yaml`）。実 DB のコピーでの比較はオーナーのローカル作業として §6 に積む。シナリオに `db: real` を書かない（ランナーが拒否する）。
- スライスを閉じる条件：担当エンドポイント / ツールごとに正常・境界・異常のシナリオ、説明のつかない差分 0、更新系の後の db-diff 0、Rust のユニットテスト（Python から書き出したベクタ）、自己レビュー。
- Python の挙動を知るにはソース（`backend/`）とオラクルの応答を見る。推測で書かない。

### 5.5 自己レビューと報告

- 終える前に、自分の diff を読み直す：バグ互換になっているか、台帳に無い差分が無いか、パニックやロックの持ちすぎが無いか、テストが Linux / Windows で通るか。
- PR 本文（または引き継ぎメモ）に、何をしたか、どう確かめたか、残っていること、オーナーの判断や実機確認が要ることを書く。実機確認は §6 に追記する。

### 5.6 公開リポジトリでの禁止事項

- **個人データをコミットしない**：オーナーの曲名・アーティスト・パス、DB や rekordbox の中身、ライブラリや再生履歴の件数や統計、ユーザー名、ホスト名、IP、メールアドレス、絶対パス。実データから得た数値はローカルのレポートにだけ置き、リポジトリには入れない（必要なら定性的に書く）。
- テストのフィクスチャは合成データだけにする。
- 秘密情報（rekordbox の鍵の値、API キー、トークン）をコード、ログ、テスト、エラー文に出さない。

---

## 6. オーナー確認リスト（途中で質問せず、ここに積む）

オーナーは、試聴・DDJ・見た目の確認をすべての作業が終わってからまとめて行います。途中で質問せず、必要な確認をここに追記してください（何を、どの手順で、合格の基準は何か）。

| # | 確認内容 | 前提 | 環境 |
|---|---|---|---|
| 1 | Mixxx での暫定 BEAT SYNC の位相合わせを 1 回聴いて判断する | release-integration | Mac |
| 2 | DB コピーで `assist_replay_eval`（emotional / bright / wordplay を含む）を回し、キーの出どころを rekordbox に切り替えてよいか判断する | release-integration | Mac |
| 3 | 実 DB のコピーでパリティ（読み取り系の大量リクエストと、普段の操作のキャプチャ再生）を回し、説明のつかない差分 0 を確認する | 各スライス | Mac |
| 4 | タグ書込みを lofty に替えた結果を rekordbox・ミュージック・Finder で読めるか確認する（D-07） | S8 | Mac |
| 5 | Rust バックエンドの同梱アプリで smoke と手動チェックリスト（起動、Library、Play と録音、Assist、USB、backup / restore、Junction、終了時の CHECKPOINT） | S13 | Mac / Win |
| 6 | 自前解析の評価レポート（rekordbox データを参照、ローカルのみ）と、見た目の比較（フレーズ色、3 バンド波形、キー表記、Traffic Light） | RB-30〜RB-34 | Mac |
| 7 | BlackHole 適合スイートを Rust エンジンで実行し、Mixxx のベースラインと比較。keylock と FX の ABX、DDJ の手触り | W2-26 | Mac |
| 8 | Junction の B2B（Rust⇄Rust、Rust⇄C++、スマホの Lite、フェーダースタート、2 時間の運用） | W2-47 | Mac / Win |
| 9 | 実ライブラリで `/api/metadata` のタグ（title / artist / album / artwork）を Python（TinyTag 2.x）と Rust（lofty）で比べる。手順：DB コピーで両方を起動し、全曲の `GET /api/metadata?track_id=` を流して差分 0（合成音源では一致を確認済み。ID3 の複数値、APE、WAV の INFO、AIFF は未確認） | S3 | Mac |
| 10 | `/api/stream` の Content-Type（D-16）。Mac の旧アプリと Rust 版で mp3 / m4a / flac / wav / aiff の値が一致すること、Windows で再生に支障がないこと | S3 | Mac / Win |

---

## 7. オーナーの判断待ち（既定値で進めてよい）

判断が出るまでは、既定値（バグ互換か、下の「既定」）で進め、`deviations.yaml` には `proposed` で記録します。

1. Mirror の 4 ルートと UI の削除（D-08）。
2. MCP で例外の理由を表示する（D-03）、Junction の廃止 5 ツールの削除（D-09）。
3. USB の snapshot_hash をバイト互換にするか、serializer_version=2 にするか。
4. 録音の書き出しを WAV / FLAC だけにする（D-04）。`/api/stream` の制限範囲。save / reveal の Tauri 移行。
5. S11 の Analyzer を暫定の Python ワーカーにして、切替を Rust 解析の完了から切り離すか。
6. 切替リリースに Python へのフォールバックを同梱するか。
7. 時刻はローカルの naive のまま（既定・推奨）か、UTC に移すか。
8. 低価値のルートを DROP するか（backend-port.md §2.5）。
9. 切替時に Mixxx をフォールバックとして同梱するか（既定：同梱しない）。
10. Junction を W6 に切り替える時期（既定：切替から十分たち、相手が全員更新してから）。
11. サーバー方式のシグナリング（`services/junction-signaling`）の廃止（既定：廃止、Rust に移植しない）。

なお、自前のキー検出は「作るかどうか」の判断事項ではなくなりました。rekordbox がオプションになったので、rekordbox を持たない利用者のために必須です（RB-33）。
