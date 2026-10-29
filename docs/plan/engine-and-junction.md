# Rust オーディオエンジンと Junction の移植

Mixxx ベースのエンジンホスト（`native/mixxx-engine-host`）を自作の Rust エンジン `plumdeck-engine` に置き換え、C++ の Junction（`native/mixxx-engine-host/src/junction`）を Rust に移植してエンジンに統合する計画です。オーナー決定は [README.md](README.md) §2 を見てください。

**受け入れゲート**は、release-integration で入った **BlackHole 適合スイート**（`native/mixxx-engine-host/tests/conformance/`）です。同じスイートを Mixxx ホストと Rust エンジンの両方にかけ、Rust が Mixxx のベースライン以上であることを確かめてから切り替えます。

---

## 0. 前提と原則

- **D2**：Mixxx は切替まで出荷物に残す。Rust エンジンと Rust 版 Junction は**既存のエンジン契約の裏で並行開発**し、パリティを確認したら一度に「Rust のみ」へ切り替える。**Junction は一度も無効化しない。**
- **D1 と所有**：DJ/DSP のロジック（keylock、EQ、FX、スクラッチ、シンク、vinyl）はすべて自作。基盤は許諾型か MPL-2.0（cpal、symphonia、libopus、rtrb など）。
- **契約先行**：新しい op やテレメトリは、まず Rust との契約として `src/services/dj-engine/protocol.ts`（と `src/types/dj-engine.ts`）に定義する。Mixxx ホストは実装できる範囲だけを capability で宣言し、UI は capability を見て表示を切り替える。UI 側の作業はエンジンを置き換えても無駄にならない。
- **「押しても何も起きない操作」をゼロにする**：未対応の機能は表示しないのを既定にし、理由は設定かツールチップで示す。
- **rekordbox 同等の挙動、自前のアセット**：機能、挙動、配置、操作順、色の意味、ショートカットの配列は rekordbox に合わせ、ロゴ・アイコン・画像・色の値・メーカー固有の名前は使わない。

### 0.1 現在の構成

```
plumdeck.app（Tauri / Rust）
 ├─ Python サイドカー（FastAPI + DuckDB + MCP + 解析）     ← backend-port.md で置き換え
 └─ PlumdeckMixxxHost（Mixxx 2.5 系 + Qt + C++ Junction） ← NDJSON v1（protocol.ts）
     src-tauri/src/dj_engine/supervisor.rs が起動・監視する
 （参考）native/dj-engine-host：Rust の小さなシミュレータ（2 デッキ、op の一部だけ）
```

### 0.2 最終形

```
plumdeck（Tauri + Rust バックエンド in-process + 解析）
 ├─ 子プロセス：同じバイナリの --worker（デコーダのクラッシュを隔離）
 └─ plumdeck-engine（Rust、externalBin。リアルタイム処理を隔離するため別プロセス。
                     Junction-rs を同じプロセス内に持つ。cpal：CoreAudio / WASAPI 共有モード）
```

Windows の音声は WASAPI 共有モードのみ（ASIO は使わない）。

### 0.3 crate（すべて `src-tauri/crates/`）

| crate | 役割 |
|---|---|
| `plumdeck-decode` | ヘッダ解析、symphonia デコード、basis 整列、逐次 PcmPages（実装済み。OS フォールバックと Opus は未） |
| `plumdeck-dsp` | リサンプラ（可変比、ASRC の核）、窓、STFT、biquad、RMS / peak、補間カーネル |
| `plumdeck-engine-*`、`plumdeck-engine`（bin） | 契約、描画（render）、入出力、波形、perf-input、ホスト |
| `junction-seam` | **エンジンと Junction の唯一の境界**（依存は serde_json だけ） |
| `junction-core` / `-transport` / `-runtime` / `-testkit` / `-peer` | Junction-rs |
| `tools/*` | golden、keylock-ref（開発専用）、bench、fixtures-gen |

### 0.4 クリーンルームの規則

- 実装の作業では、Mixxx 由来の `native/mixxx-engine-host/src/plumdeck_vinyl_scaler.*` を sparse-checkout で除外して開かない。Mixxx の `effects/` やエンジンのソースも読まない。
- 読んでよいのは、クリーンルーム仕様（`docs/engine/spec/*`、W2-05 / W2-06 で作る）と、**オーナー自身が書いたコード**：`vinyl_kernel.h`、`vinyl_convolution.h`、`scratch_deck.h`、`scratch_prediction.h`、`ddj_beat_processors.h`、`beat_fx.h`、`sampler_bank.h`、`performance_input.h`、`slip_return.h`、`audio_meter.h`、`deck_telemetry.h`、`waveform/*`、`host.cpp`、`mixxx_backend.cpp` のオーナー記述部分。
- FX は教科書の手法（『DAFX』、Zavalishin の VA フィルタなど）で書く。
- ブラックボックスで測った特性（Mixxx の EQ カーブ、FX の T60 など）への当てはめは D1 の範囲内とみなす（測定値だけを使い、コードは見ない）。
- CI の `check-cleanroom.mjs`（W2-05 で作る）で、クリーンルームの PR の差分に禁止ソースの識別子が無いことを検査する。
- DDJ-400 のマッピングは Mixxx の GPL の XML が出典なので、自前で作り直す。

### 0.5 時間軸（timeline basis）

| basis | 定義 |
|---|---|
| `legacy-mixxx-mac-v1` / `legacy-mixxx-win-v1` | 各 OS の Mixxx ホストが出す frame 0 |
| `canonical-v2` | エンコーダ遅延とパディングを除いた最初の実サンプルを 0。遅延情報の無いファイル（MP3 ケース A など）は「デコーダ出力の先頭を 0」 |
| `essentia-ffmpeg28-v1` / `ffmpeg-cli-win-v1` | 既存の解析行の基準 |

- 切替までは、DB の位置データはすべて「そのプラットフォームの legacy basis」で保存する。Rust のデコーダは canonical でデコードし、`contracts/timeline/decoder-timing-v2.json` の Δ を使って legacy にずらして返す。
- 正解値は Mixxx ホスト自身のデコード結果（ヘッドレスの `waveform.requestRange{detail:'pcm'}`）。
- canonical-v2 への一括移行は切替の 1 リリース後（W3-03）。エンジン依存の値（44.1k / 256 frames、rekordbox の時刻補正表）はバックエンドの `engine_profile` に隔離してある（[backend-port.md](backend-port.md) §1.9）。

### 0.6 エンジンと Junction の境界（`junction-seam`）

- **オーディオ側**：型状態で順序を強制する：`begin` → `render_junction_input` → ミックス → `on_block`（`JBlock` を消費）→ `BoothGate` → 試聴の混合。`discontinuity()` を必ず通知する。
- **制御側**：J のセッションスレッドから呼ぶ `JunctionEngine`（`&self`、`Send + Sync`）を E が「ハンドル」として実装する。読み取り系は制御スレッドが 5ms ごとに更新するロックフリーのスナップショット（ArcSwap）から、変更系は有界チャネルで制御スレッドへ送り oneshot で応答（50ms でタイムアウト）。E の NDJSON ディスパッチャは `JunctionControl::command / authorize` を制御スレッドから同期で呼ぶ。

---

## 1. BlackHole 適合スイート（受け入れゲート）

- 場所：`native/mixxx-engine-host/tests/conformance/`（`harness.mjs`、`analysis.mjs`、`play-conformance.test.mjs`、`analysis.test.mjs`）。
- 実行：`PLUMDECK_TEST_HOST` にホストのコマンドを指定して有効化する（未指定ならスキップ）。`PLUMDECK_CONFORMANCE_REPORT` に結果の JSON、`PLUMDECK_CONFORMANCE_LABEL` で実行のラベル、`PLUMDECK_CONFORMANCE_ONLY` で絞り込み。録音・波形キャッシュ・一時ファイルは `PLUMDECK_CONFORMANCE_TMP` などで作業ディレクトリ配下に置く。
- 安全：**出力が BlackHole 2ch に適用されていなければ音を出さない**（システムの既定出力は変えない）。master gain は 0.2〜0.5、素材のピークは −6 dBFS 以下。WAV は測り終えたらすぐ削除。
- 素材（合成）：120 / 128 BPM のパルス（拍頭のクリック＋80 Hz / 1 kHz / 8 kHz のバースト）、ホワイトノイズ、1 kHz サイン、疎なクリック、キー情報つきのトーン。
- 測るもの：帯域の振幅、無音率、包絡の自己相関の周期、位相（クリックと拍グリッドの差）、ドライとの残差、L/R の差、クリック雑音、応答時間、メーター、sync の leader テレメトリ、一度だけの位相シンク、KEY SYNC、SLIP 復帰、ヘッドホンの op、ループ範囲、クロスフェーダーカーブ、そして FX の品質基準 QB-1〜QB-9（§3.3）。
- CI では解析ヘルパーの自己テスト（`analysis.test.mjs`）だけを回す。音を出す本体は macOS（BlackHole を入れた実機か、ラベル付きの macOS ランナー）が要る。
- **Rust エンジンの受け入れ**：同じ NDJSON プロトコルを話すので、`PLUMDECK_TEST_HOST` を `plumdeck-engine` に向ければ同じスイートが走る。Mixxx ホストで取ったベースラインのレポートと並べ、すべての項目で同等以上であることを切替の条件にする。
- **クラウドのセッション**は BlackHole を使えないので、`NullBackend` / `OfflineBackend`（§2 W2-08）でオフラインに描画した PCM に、同じ解析ヘルパーをかける形のテストを並行して用意する。BlackHole での実行はオーナー確認かラベル付きの macOS ランナーに回す。

---

## 2. エンジン（E）の作業項目

| ID | 内容 | 受け入れ基準の要点 |
|---|---|---|
| W2-01 | エンジン契約 v1 の機械可読化（`contracts/dj-engine/v1/`：op、イベント、capability、エラーコード、定数、JSON Schema、バイナリ形式、例）とドリフト検査（host.cpp などからリテラルを抽出して照合） | 未分類の op 0、op を 1 つ足すと検出。**一部済**：`contracts/dj-engine/v1/contract.json`（op 115、イベント 10、capability 50、エラー 15、定数、分類と実装側）と `tools/engine-contract/contract.mjs`（CI でドリフト検査）。op ごとの params の JSON Schema、バイナリ形式、例は W2-09 で追加 |
| W2-02 | BlackHole の CI スパイク（macOS ランナーで BlackHole を入れ、既存のタイミングテストを 1 本） | 成否を記録。失敗ならゴールデン採取をオーナーの実機に回す |
| W2-03 | 両エンジン共通のコンフォーマンス・スイートの整理（既存の音声テストと §1 のスイートを `requires(capability)` でまとめ、capability ごとの集計） | シミュレータで fail 0、assert と閾値が移行前と同じ |
| W2-04 | ゴールデン採取の基盤（G01〜G22 のシナリオ：transport、varispeed、keylock、EQ、filter、カーブ、PFL、パッド FX、Color FX、Beat FX、loop、hotcue、slip、sync、scratch、MIDI、sampler、マイク、録音、波形、デコーダ時刻、テレメトリ）。合成音源だけを公開し、私的なゴールデンはオーナーの Mac の中だけ | manifest の sha が確定 |
| W2-05 | クリーンルーム仕様①：transport の意味論（sync とリーダー、quantize、loop、slip、reverse / censor、beatjump、hotcue）と `check-cleanroom.mjs` | 全性質にテストベクトルか出典 |
| W2-06 | クリーンルーム仕様②：ミキサーのカーブ、vinyl、FX の測定値（この WP の担当だけが GPL の vinyl scaler と実測トレースを読む） | GPL の識別子との一致 0 |
| W2-07 | RT コア：`Renderer::render`（256 フレーム以下のサブブロック）、`rtrb` のコマンド（Copy、64B 以下）、Box / Arc は受け渡しリングと返却リングで制御スレッドへ返して drop、clippy の disallowed-methods | 10 秒のオフライン描画で割り当て 0（`assert_no_alloc`）、負のテストあり |
| W2-08 | バックエンド 3 種：`NullBackend`（実時間ペース、仮想デバイス、入力 WAV）、`OfflineBackend`（`engine.render{frames}`）、`CpalBackend`（多チャンネル、固定 256 フレーム優先、入力、デバイス喪失の通知） | 3 OS でビルド、null と offline の決定性 |
| W2-09 | NDJSON ホストのセッション層（session.hello、stale id、1 MiB の上限、未実装 op は `unsupported_operation`、ping、snapshot、clock.probe、audioHealth、20ms のティッカー） | プロトコルのコンフォーマンスが null で pass |
| W2-10 | 補間カーネル（オーナーの vinyl_kernel / vinyl_convolution を `plumdeck-dsp::interp` に） | 通過域 <0.5 dB、阻止域 <−60 dB |
| W2-11 | デッキの transport と逐次ロード（位置は f64、速度はブロック内で線形ランプ、PcmPages から畳み込みで読む、先頭とキュー付近が揃ったら `deck.loaded`、未デコード位置への seek は待たせる） | rate 1.0 / 0.96 / 1.08 で 60 秒後の位置が ±0 フレーム |
| W2-12 | loop、hotcue、beatjump、quantize、sync、slip、reverse、censor（W2-05 の仕様どおり） | ループ 100 回の折り返し後 ±0 |
| W2-13 | テレメトリ（`deck.clock.v2`、position の差分、`deck.timing.trace`） | 周期 20±5ms、dropped 0 |
| W2-14 | EQ とフィルタ（RBJ シェルフとピーキング、LR4 の kill、TPT の SVF。ゴールデンへの当てはめで定数を決める） | 30 Hz〜16 kHz で ±0.5 dB、kill は Mixxx −3 dB 以内か深い |
| W2-15 | ストリップ（trim → EQ → color / filter → パッド FX → PFL タップ → フェーダー → クロスフェーダー）、main バス、PFL、出力ルーティング（4ch 機器）、デバイスの一覧と設定（旧 env 名も受け付ける） | dj-controls と PFL が pass |
| W2-16 | 録音（マスターゲイン後をタップ、専用リング、WAV 16bit か FLAC） | frameCount が WAV と完全一致、FLAC の往復一致 |
| W2-17 | 波形 Pyramid と DJWV のバイト互換（オーナーのコードを移植。パリティのため。切替後に PDW3 へ：RB-43） | 合成 PCM でバイト一致、`src-tauri/src/waveform.rs` が読める |
| W2-18 | スクラッチとビニール（オーナーの制御則を移植、着地式を新しいリーダーに合わせて導き直す） | 位置誤差 ≤1e-6 フレーム、着地 6 コールバック以内・2ms 以下 |
| W2-19 | PerformanceInput（48B の MIDI レコード、macOS は UDS・Windows は名前付きパイプ、トークン認証、DDJ-1000 のデコード） | native-input と ddj1000 が pass |
| W2-20 | サンプラー（4 バンク×16）、マイク（別デバイス可、可変比リサンプラと PI 制御）、ダッキング | ±0.1% のドリフト 10 分でアンダー / オーバーラン 0 |
| W2-21 | FX①：オーナー自作の DDJ 系 8 種（lowcutecho、mtdelay、spiral、enigmajet、sliproll、roll、mobiussaw、mobiustri）の移植 | 切り離した C++ 版と −80 dB 以内、割り当て 0 |
| W2-22 | FX②：教科書手法の FX（echo、reverb（FDN）、tremolo、flanger、phaser、filter、bitcrusher、distortion、autopan、moog ladder、whitenoise、sweep、vinyl brake、back spin、rev roll、slip loop など） | 遅延 ±1ms、T60 ±15%、LFO ±2%、wet/dry ±1 dB |
| W2-23 | FX③：Beat FX の送り先（A〜D、master、mic、sampler）、AUTO テンポ（リーダー → 再生中のデッキ → 120）と手動 40〜300 BPM、RELEASE FX の状態機械 | first-fx、colorfx-audio、beat-fx-audio が pass |
| W2-24 | keylock（自作の位相ボコーダ：STFT 2048、ホップ 512、identity phase locking、トランジェントで位相リセット）とピッチシフト。参照出力は開発専用ツールで作り、出荷物に入れない | 0.75〜1.25 でピッチ誤差 <1 cent、5 分で位置ドリフト 0、ABX でオーナー判定 |
| W2-25 | supervisor でのエンジン選択（`PLUMDECK_DJ_ENGINE` → 設定 → 既定 mixxx）とプレビュー版（切り替えは `PLUMDECK_ENGINE_PREVIEW=1` のときだけ表示） | 両 OS のプレビュー版で smoke が pass |
| W2-26 | パリティ・キャンペーン（全コンフォーマンス、全ゴールデン、null の長時間ソーク、最悪ケースのベンチ：4 デッキすべて keylock・パッド FX・reverb・サンプラー・マイク・J） | fail 0、ゴールデンの許容外 0 か免除あり、最悪ケースがコールバック周期の 25% 以内、RSS ≤400 MB |

---

## 3. Play モードの仕様で Rust エンジンに入れるもの

release-integration で、UI の修正と Mixxx ホストで小さく直せるもの（H-1〜H-12 相当）は済んでいます。Mixxx の上で作ると捨てることになるものは、次のとおり Rust エンジンの仕様にします。

### 3.1 必須の機能

1. **シンク**：BPM と位相を継続的に合わせる（≤5ms を 60 秒維持）。SYNC RATE ×2 / ×½。MASTER と BEAT SYNC の独立。leader をテレメトリでプッシュ。拍と小節頭まで合わせる BEAT SYNC は RB-41。
2. **クオンタイズ**：デッキごと / 全デッキ連動の Q を、ホットキュー・ループ・リバース・サンプラーのループ・CUE に効かせる。即時（位置補正）と次の拍で実行の 2 モード。
3. **SLIP**：ゴーストの位置を 40 Hz 以上でテレメトリに出す。あらゆる演奏操作の解除で裏の位置に戻る。
4. **キュー**：Back Cue、Cue Point Sampler、自動 CUE、HOT CUE LOOP、Gate Cue、slip hot cue。
5. **FX エンジン**：PAD FX・BEAT FX・Color FX を同じ基盤に。デッキごとの PAD FX スロット群（同時押し、同じ FX で拍違いは最後に押したもの優先、HOLD、押しながら ◀▶ で拍を半分 / 倍）。拍と小節に同期した LFO とゲートの位相。テール。RELEASE FX（ECHO OUT / BACK SPIN / VINYL BRAKE）。FX1 / FX2 × SINGLE / MULTI、SNAPSHOT、TAP。
6. **Color FX**：複数種、左右非対称、Multi モード、Color FX への RELEASE FX の適用。
7. **ミキサー**：EQ / ISOLATOR と EQ Type、チャンネルフェーダーとクロスフェーダーのカーブ、フェーダー前のチャンネルメーター、マスターとヘッドホンのメーター、2ch 機器でもヘッドホンを分けられる出力ルーティング。
8. **テンポとループ**：WIDE ±100%（−100% で停止）。ループ 1/64〜512 拍、BEAT JUMP は FINE〜128 拍。
9. **パッドモード**：SLICER（スリップ前提）、ACT. CENSR、SEQ. CALL とシーケンサー、MEMORY CUE、KEYBOARD（RB-42）。サンプラーの Oneshot / Loop / Gate / SYNC とスロットごとの音量。状態はポーリングでなくプッシュ。STEMS は後で代替手段（README §2.9）。
10. **その他**：エンジンからのキー出力、3 バンドの波形タイル（PDW3）、キーシフト中も MT の状態を保つ。

### 3.2 PAD FX の既定（オーナー決定）

- 全デッキで 2 バンクを共有する。
  - Bank 1（パッド 1〜8）：ROLL 1/2、SWEEP 80、FLANGER 16、V.BRAKE（release）3/4、ECHO 1/4、ECHO 1/2、REVERB 50、ECHO（release）1/2
  - Bank 2：SLIP LOOP 1/16、1/8、1/4、1/2、REV ROLL 1/16、1/8、1/4、1/2（param 50）
- スロットの定義（UI に保存し、Rust の契約にもする）：`{ kind: "beat"|"color"|"release", effect, beats?, params, hold, color? }`。DEPTH はスロットごと。挿入型（FILTER / CRUSH / TRANS / ROLL）は D/W 1.0 固定。
- 2 つのビュー：通常の「使用中」ビューと、歯車で切り替えるインライン編集ビュー（パッドごとに FX の種類・拍・パラメータのドロップダウン）。編集ビューは未実装。
- 効いている間はパッド全体を種類の色で塗り、拍は大きな分数で表示する。名前は自前の短い名前。

### 3.3 FX の品質基準（PAD FX と BEAT FX 共通。§1 のスイートで測る）

- **QB-1 拍の周期**：delay / gate / roll の周期が「拍数 × 60 / 実効 BPM」の ±1ms。テンポを変えたら 1 拍以内に追従。
- **QB-2 位相**：TRANS / ROLL / LFO の立ち上がりが、エフェクトの拍単位のグリッドに対して ±3ms 以内。
- **QB-3 原音**：挿入型を全開にしたとき原音の漏れ −40 dB 以下。ECHO / REVERB 系は LEVEL に関係なく原音が ±1 dB。LEVEL 0 でドライとの残差 −60 dB 以下。
- **QB-4 テール**：ECHO / SPIRAL / REVERB は離しても残響が続く（ECHO は 2 拍以上 −40 dB を上回り、REVERB は −60 dB まで単調減衰）。
- **QB-5 クリック**：ON/OFF や切替で帯域外エネルギー −50 dB 以下。
- **QB-6 同時押し**：1 デッキで 3 つ同時でも xruns 0、3 つとも効いている。
- **QB-7 応答**：コマンドの往復 p90 ≤25ms。押してから効くまで 1 バッファ＋5ms 以内。初回ロードで 100ms を超えない。
- **QB-8 RELEASE**：押したら他の FX が 1 バッファ以内に止まり、離したら原音が 1 拍以内に 0 dB ±0.5 に戻る。
- **QB-9 停止中**：入力が無音なら出力も無音（−90 dBFS 以下）。

### 3.4 UI 側の残り（エンジンに依存しない部分を含む）

- 波形：rate 連動、共通ズーム（+ / − / RST）、3 バンドで統一（PDW3、RB-20）、ホットキュー色をパッドと共通化、CUE・メモリーキュー・ループの印。
- ブラウザ：Traffic Light（既定 ON）、ΔBPM、MASTER 基準のフィルタ、再生済みの着色、推薦の基準曲の選択、キー相性の推薦フィルタ（既定 OFF）。
- 色の意味（値は自前のトークン）：CUE はオレンジ（波形）/ 赤（プラッター、SLIP 中は黄）、再生位置は赤の縦線、拍線は白（小節頭は赤、編集中は青）、メモリーキューは赤、保存したループ / 有効なループは黄 / 赤、Q / MT / SLIP の ON は赤、FX パラメータの ON・PAD FX の編集・RELEASE 中は青、ホットキューは選べるパレットでパッドと波形が同じ色。
- 見やすさの最低基準：文字 10px 以上（よく使うラベルは 11px 以上）、当たり判定 24px 以上（パッドは 44px 以上）、コントラスト 4.5:1 以上、アクティブデッキは上辺をデッキ色で塗り番号を反転、無効な操作は理由をツールチップと近くのキャプションに。
- 「押しても何も起きない操作ゼロ」のテスト：SoftwareDeck・PerformancePads・MixerPanel・FX パネルを capability の組み合わせごとにモック client で描画し、有効なボタンは必ず op を出すか状態が変わる、無効なボタンには必ず理由がある、を確かめる。

---

## 4. Junction の Rust 移植（J）

> **2026-10-29 オーナー決定：Junction v2 に置き換え。** 旧 Junction（C++ の WebRTC ランタイム、W5/W6 ワイヤ、STANDBY/READY/ON AIR/OUTGOING とフェーダースタート、Lite ホスト）は忠実に移植しない。以下の §4.1〜§4.2（W2-30〜W2-47）は v2 で置き換えられた旧計画として残す。v2 の構成は §4.0。

### 4.0 Junction v2（実装済み・実回線は未確認）

- **用途**：離れた場所の約 5 人の DJ（B2B を含む）が 1 本の途切れないセットをつなぎ、ライブ配信に出す。主催者が plumdeck で**ホスト**になり、ホストの plumdeck が配信の音（Program）を音声デバイス（OBS へは BlackHole など）へ出す。数秒の配信遅延は許容。
- **トポロジー**：ホスト中心の star。WebRTC は WebView／ブラウザの `RTCPeerConnection`（Opus ステレオ 192 kbps の sendrecv 1 本＋DataChannel `jlink`）。シグナリングは share-musics の `junction_*`（offer/answer、ICE 収集完了後の SDP を 1 往復）か、招待コード／応答コード（`JLINK2.`）の手渡し。旧プロトコルとの互換は無い。
- **順番**：ホストが順番・ON AIR・NEXT（明示の指名か順番の次）・前のDJ を持ち、変更のたびに状態全体を配る。NEXT はホスト・ON AIR・次の DJ が押せる。デッキのロックは無い（`src/services/junction-v2/core/turns.ts`）。
- **送出と J フィード**：ON AIR と次の DJ は J を含むミックス全体（full）、それ以外は自分のデッキだけ（local、J とマイクを含まない）を送る。J デッキには、ON AIR の DJ には前の DJ の残りの曲、前の DJ には何も、それ以外には ON AIR の DJ の音をホストが中継する。誰も自分の音を J で聴き返さない（テストで全状態を検査）。ON AIR の DJ が J を下げ切って 1.5 秒たつと前の DJ の中継を止める。
- **Program とつなぎ目**：ホストは受信したすべての音を 1 つの AudioContext で同じクロックに載せ、ON AIR の音を固定遅延 D（3 秒）で読む。NEXT のとき、次の DJ のミックスに入っている「前の DJ の音」の遅れ L（ホスト→次の DJ→ホストの一巡）を、タイムスタンプ（RTT と各端末の報告するバッファ遅延）を事前分布にして、音声そのものの相互相関（GCC-PHAT＋正規化相関、サンプル精度）で測り、読み位置を L だけ進めて 30 ms の線形クロスフェードでつなぐ。前の DJ の音は繰り返しも欠けも無く続く。使った L は最大 0.2 % の再生速度調整で数分かけて D に戻す。詳細は `core/program.ts` の冒頭コメント。
- **アプリ内の音声経路**：WebView（AudioWorklet）とエンジンの間はループバックの WebSocket（`junction::Link`、`jlink.*` op、トークン付き）。WebRTC の受信音→J デッキ（`[Auxiliary1]`、通常のチャンネルとしてレベル・EQ・クロスフェーダー・CUE）、エンジンのミックス（full＝マスターつまみ前のメイン、local＝J とマイクを除くローカルバス）→WebRTC。PCM は UI スレッドを通らず、AudioWorklet と I/O ワーカーの MessagePort で流れる。
- **Lite**：ゲストとしてだけ参加。MixerEngine の JUNCTION チャンネルと送出（`setJunctionTail`）をそのまま使う。

### 4.1 要件（オーナー決定、[README.md](README.md) §2.10）

- 操作する DJ は常に 1 人。出ていく DJ の音は、入ってくる DJ の JUNCTION デッキ（レベル、EQ、クロスフェーダー、CUE）で鳴り続け、入ってくる DJ がフェードアウトしたときに止まる。**操作者の交代と音の停止は別の手順。**
- Mac → スマホ方向は同期のずれを許容。スマホ（PlumDeck Lite、別リポジトリ）はクロスフェーダーとレベルフェーダーだけ。
- UI は JUNCTION レーン 1 本、グリッド範囲の色分けでどちらの曲かを示す。
- フェーダースタートの交代（release 済みの fader-start Junction）の挙動を保つ。資料は `docs/junction-transition/`。
- 配布済みの C++ ピアとワイヤ形式 W5 で互換を保つ（切替時点では W5 のまま。W6 は受け取る処理だけ先に入れる）。

### 4.2 作業項目

| ID | 内容 | 受け入れ基準の要点 |
|---|---|---|
| W2-30 | 仕様ベースラインと追跡台帳（`native/junction-spec/`：SPEC.md、TESTMAP.json、OPMAP.json、CONSTANTS.json、BASELINE.json、PORTING-LEDGER.md）と検査スクリプト | 既存のテストと op をすべて見つけ、追加・変更・不一致を検出。**済**：C++ の JTEST、ネイティブの Node、フロント、シグナリング、MCP のテスト 213 件、`junction.*` コマンド 51、制御メッセージ 9、定数 37 を抽出。ソース・テスト・コマンド・定数の変更、SPEC.md に無いコマンド、台帳の移植先が存在しない場合を CI（`tools/junction-spec/baseline.mjs`）で検出。不一致 2 件（`private.stop` が JunctionOp に無い、C++ のスイート名の揺れ）は OPMAP.json に記録 |
| W2-31 | C++ 参照実装の Linux ビルド、ベクタの生成、参照ピア（macOS ランナーでも生成し、Qt の版による JSON 書式の差を検査） | ctest が通る、ベクタが決定的、参照ピア同士の接続 |
| W2-32 | `junction.*` の記録（音声の要らないシナリオの要求・応答・スナップショット） | op 群の正常系と異常系が記録されている |
| W2-33 | junction-core①：ids、Qt 互換の JSON 正準化、manual_exchange（P-256 の ECDSA、W5 と W6 の decode）、session_protocol、authority、turn_state、program_mixer、ice_servers、network_settings。C++ のテストを同じ名前で移植 | 正準バイト列の sha が全件一致、署名の相互検証、Qt 非依存 |
| W2-34 | junction-core②：audio_clock、pcm_ring、producer_tap、junction_input（リサンプラは `plumdeck-dsp`）。44.1k と 48k の両方 | 割り当て 0、±100 ppm を長時間流してもフィルが範囲内 |
| W2-35 | ヘッドレス検証基盤（seed 付き NetSim と仮想時計、coturn、Chromium で動く lite-sim、FakeEngine） | 決定性、coturn の 3 方式での往復、lite-sim の自己試験 |
| W2-36 | トランスポートのスパイクと ADR（第一候補 str0m＋turn-client-proto＋Opus。だめなら webrtc-rs の rtc、TCP/TLS で落ちたら TURN クライアントを自作） | 判定 G1〜G8（ブラウザ、C++ との W5 接続、NACK、TURN の udp/tcp/tls、ID、ライセンス、サイズ、決定性）と ADR。両案とも W5 互換で落ちたらオーナー判断 |
| W2-37 | transport①：link（W5 は 2 本、W6 は 1 本、Lite は sendrecv 1 本）、identity（P-256）、ネットワーク駆動 | C++ 参照ピアと W5 で接続、アイドルでも保持 |
| W2-38 | transport②：RTP（PT111、SSRC とタイムスタンプの書き換え）、SR / NACK、Opus（48 kHz、960 フレーム、CBR、VBR と DTX オフ） | 5% の欠落で修復後 ≤0.5%、エポック切替で途切れ ≤60ms |
| W2-39 | transport③：候補収集と TURN（UDP / TCP / TLS、REST の資格情報、強制中継） | Rust⇄Rust と Rust⇄C++ がすべての経路で開通 |
| W2-40 | `junction-seam` と FakeEngine（§0.6、スレッドモデル、E への要求の README、trybuild によるコンパイル失敗の試験） | 透過性 ≤1e-6、LOCAL NEXT に J とマイクが入らない |
| W2-41 | エンジン側の Junction 配線（J ストリップ、LOCAL NEXT バス、Program タップ（マスターゲイン前、マイク込み）、BoothGate、試聴の挿入点、ハンドル、ディスパッチャでの `authorize`） | 入力が同じコールバックの Program タップに出る |
| W2-42 | runtime①：sans-IO の Session と driver、op 群 A、交換の状態機械、制御のエンベロープ（`exchangeMode:'server'` は日本語で拒否） | NetSim で connected まで決定的、C++ と hello とスナップショットの受理 |
| W2-43 | runtime②：交代、権限、テール、継ぎ目、Lite の語彙 | ジッタ・ppm・欠落の条件で継ぎ目の誤差 ≤48 フレーム、二重再生 0、フェーダースタートが 1 回だけ発火 |
| W2-44 | runtime③：Program 出力（会場遅延、ASRC、24bit WAV）、ネットワーク設定の保存（macOS はキーチェーン、Windows は DPAPI）、スリープ抑止 | VirtualSink での試験、保存の往復 |
| W2-45 | 版（W5 / W6）と互換マトリクス：ホスト {Rust W5, Rust W6, C++} × ゲスト {Rust, C++, lite-sim} × 経路 {直接, turn-udp, tcp, tls} | 成功すべき組み合わせがすべて成功、W6 ホスト × C++ ゲストは決まった文言で失敗 |
| W2-46 | `junction-peer`：実回線の確認用 CLI（出荷しない） | 計測 JSON（link、候補、rtt、欠落、修復、トーン、継ぎ目） |
| W2-47 | エンジンへの実ランタイムの統合と、Junction のフロントエンド試験を Rust エンジンに対して実行 | 長時間のブロックで描画フレームが一致 |

---

## 5. 切替（Phase 3）

| ID | 内容 | 受け入れ基準 |
|---|---|---|
| W3-01 | 共同の切替（Rust のみ）：supervisor の既定を Rust に、release の本番ビルドに `plumdeck-engine` を入れ、Mixxx ホストと C++ Junction を出荷物から外す（ソースは W3-02 まで残す）。緊急時の戻し方（Mixxx 入りの版を作り直して公開する手順）を書く | 両 OS の成果物が Rust エンジンだけで smoke を通る。出荷物に libnice、glib、gnutls、libdatachannel、Qt が無い |
| W3-02 | Mixxx、旧シミュレータ、C++ Junction のソース削除とライセンスの最終確定（W5 のワイヤ定数だけは許可リストに残す） | 出荷物のライセンス一覧で GPL、LGPL、AGPL、CC-NC が 0 |
| W3-03 | timeline-v2 への DB 移行（位置の意味が変わるのでスキーマの版を上げる。冪等、事前バックアップ、逆変換あり。rekordbox の時刻補正 v2 を有効に） | 合成 DB で移行・2 回目・ロールバックが一致し、macOS と Windows のオフセットが同じ |
| W3-04 | Junction の W6 既定化（時期はオーナー判断）と、次の大きな版での W5 受理の削除 | 新しい既定で互換マトリクスが緑 |

### 5.1 切替を延期する条件

次のどれかに当たれば切替を延期します。

- P0 のコンフォーマンス（§1 のスイートを含む）の失敗。
- 免除されていないゴールデンの許容外。
- 長時間のソークで xrun が 1 回でもある（Mixxx が 0 の場合）。
- CPU が Mixxx の 1.5 倍を超える。
- keylock や FX の ABX で、オーナーが「劣る」と判定したものが一定数を超える。
- DDJ の手触りの劣化。
- Junction の受け入れ基準の未達。
- rekordbox の CUE を抜き取りで確認して、1 曲でも 1ms を超えるずれ。

keylock が届かなければ、マルチ解像度の v2 を追加で作ります。

---

## 6. オーナー確認（まとめて最後に。README §6 に積む）

- Mixxx での暫定 BEAT SYNC の位相合わせを 1 回聴く（release-integration）。
- プレビュー版で、Rust エンジンに切り替えて実ライブラリの曲をロードし、2 デッキでミックス。rekordbox の CUE が Mixxx と Rust で同じ位置か、MP3 の種類と m4a で聴き比べる。内蔵出力と DDJ-1000（4ch、PFL）。
- keylock の ABX（テンポ 0.94 / 1.06）、Beat FX、EQ kill、ジョグ、スクラッチ、バックスピン、着地、マイクのトークオーバー、サンプラー。
- BlackHole 適合スイートを Rust エンジンで実行し、Mixxx のベースラインと比べる。
- Junction v2（§4.0）の実回線：別々のネットワークの Mac 2〜3 台と Lite（Chrome／iPhone Safari）で接続（share-musics と招待コードの両方）、5 人での一周、NEXT のつなぎ目を配信出力（BlackHole→OBS）で聴いて二重・飛びが無いか、J を下げたときの自動解放、Program 遅延の回復（数分）、長時間の運用、ホストの WebView で RTCPeerConnection と AudioWorklet が動くこと、Windows（WebView2）でのスモーク。
- timeline-v2 の移行後、rekordbox の CUE と手で打った CUE を抜き取りで確認。
