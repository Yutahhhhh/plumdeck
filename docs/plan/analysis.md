# 解析：最小仕様（lean spec）と残りのフェーズ

「今使っている機能はすべて残し、それに要らない計算と保存はしない」ための解析仕様と、残りの作業です。rekordbox 同等の自前検出（フレーズ、小節頭、ボーカルなど）は [rekordbox-parity.md](rekordbox-parity.md) §4〜§5 で扱い、ここではその土台になる解析経路を扱います。

---

## 1. 原則

1. **値の出どころは「手動 ＞ rekordbox ＞ 自前」**。自前で計算するのは、rekordbox に無い値と、rekordbox に無い曲だけ。ただし rekordbox はオプションなので、rekordbox を持たない利用者には自前解析が主経路になり、rekordbox 同等の品質を求める。
2. **再生できるようになるまでに前景でやる処理は 3 つだけ**：タグを読む、rekordbox と照合する、1 回デコードして音色 4 種と概形の波形を出す（rekordbox に無い曲はリズムとキーも同じデコードから）。埋め込みはバックグラウンド。
3. **既存の解析済みの行は再計算しない**。残す出力の定義（今の Essentia の定義）も変えない。定義を変えるなら、Rust 化のときの全曲再計算 1 回にまとめ、Assist の replay 評価で調整し直す。
4. **ジャンルは解析しない**。rekordbox やタグのジャンルは使わず、検証済みジャンルから端末上で学習する分類器で付ける（[README.md](README.md) §2.4）。

---

## 2. 出力ごとの仕様

| 出力 | 出どころ | 精度・形式 | いつ計算するか | 最終の実装（Rust） |
|---|---|---|---|---|
| duration | タグ | 秒（f64） | 前景 | lofty（デコード長に変えると PQTZ が範囲外として弾かれる曲が出るので、タグの値を使い続ける） |
| bpm | 手動 ＞ rekordbox（djmdContent.BPM ÷ 100）＞ 自前（未登録の曲と BPM=0 の曲） | 小数 2 桁（Assist は ±3% のゲートと σ=3%） | 前景 | 一定テンポの当てはめ＋DJ 帯域への折り返し（RB-32） |
| グリッド | 手動 ＞ PQTZ（ロード時に遅延で読み、時刻補正込み）＞ 自前 | 一定テンポなら bpm＋first_beat_ms、可変なら beat_times_ms。小節頭は PQTZ の beat_number か自前の推定 | 未登録曲は前景。登録曲で rekordbox を読めないときはロード時にその場で | RB-32 |
| key | 手動 ＞ rekordbox（djmdKey.ScaleName を正規化）＞ 自前 | 主音と長短（KeyId 0〜23） | 前景 | クロマとテンプレート（RB-33。rekordbox が無い利用者のため必須） |
| energy / danceability / brightness / noisiness | 自前のみ（全曲） | 0〜1、小数 2 桁。今の定義（RMS÷0.4、DFA÷2.5、時間領域の重心、ZCR）を保つ | 前景 | 誤差 0.02 未満に較正（W1-24） |
| 概形の波形 | 自前（同じデコードから） | 今は u8×500 の絶対振幅。最終は PDW3 の全体波形（1200 列 × 3 バンド）に置き換え | 前景 | RB-31 |
| embedding | 自前の MusiCNN | float32×200 | バックグラウンドのジョブ（取り込み後に自動投入）。無い曲は採点から外し、残りの重みで正規化し直す | ISC 版の重みを ONNX にして tract（W1-27 / W1-28） |
| 詳細 3 帯域波形 | 解析の範囲外（表示するときに作る） | — | 表示するとき | PDW3 の詳細波形（rekordbox の曲は PWV7 から） |

### 2.1 計算も保存もしないもの

- 出力の列：loudness、loudness_range、spectral_flux、spectral_rolloff、contrast、key_strength、scale（key の 2 語目から作れる）。
- features_extra の中身：拍・波形の配列の JSON、旧キー（bpm_raw、loudness_range_db、spectral_rolloff_hz、spectral_flux_raw）、記録用キー。
- 外部への公開：bpm_confidence、`/api/metadata` の beat_positions。
- DB の列、TrackRead、CSV のヘッダは既定値（−60、0.0、''）を入れて互換のため残す。

### 2.2 features_extra の許可リスト

`analysis_components`（値の出どころ `sources` もここに記録）、`playback_grid` / `playback_grid_version`（自前グリッドの曲だけ）、`analysis_bpm`（自前で出した値の退避先）、ほか release-integration で許可リストに加えたもの（CSV 書き出しとグリッドのフォールバックが読むキー）。現在の正は `backend/` の許可リストの定義なので、移植時はそれに合わせる。

### 2.3 状態は 2 つだけ

- **解析済み**：duration > 0、bpm > 0、音色 4 種と波形がある。未登録の曲はさらに自前の拍がある。Dashboard、Library、Play のすべてでこの 1 つの定義を使う。音色が既定値のままの曲は「未完了」。
- **埋め込み**：「あり」か「計算待ち」を別に表示する。完了の条件に入れない。
- 解析方法の選択（自動・軽量・詳細）はなくす。
- Dashboard の「要対応」は、この定義に沿って未解析・失敗・音色が既定値・埋め込み待ち・古い解析を数え、ワンクリックで解析 / 再試行できるようにする（[README.md](README.md) §2.5）。

### 2.4 1 曲の処理

- **analyze(path, need)**：親（バックエンド）が rekordbox と照合して need を決める。登録曲で BPM があれば need は timbre と waveform だけ。未登録の曲なら rhythm を加え、キーが無ければ key も加える。デコードは 1 回だけ。
- **embed(path)**：バックグラウンドのジョブ（only_outdated）。前景の取り込みを優先する。
- **ワーカー**：JSON 行 V1 の 1 方式で起動し、締め切りは曲の長さに比例する 1 つだけ。

### 2.5 精度の根拠

- BPM：Assist の判定は ±3% と σ=3%。ms 単位の精度が要るのは Play のグリッドだけで、rekordbox がある曲では PQTZ がまかなう。
- キー：推薦のスコアでキーの重みは 0（オーナーはキーを合わせない）。表示、KEY SYNC、Traffic Light、キー相性の推薦フィルタ、一部のプリセットの方向付け（emotional / bright / wordplay）に使う。キーが分からない曲は残りの重みで正規化し直す。
- 音色：小数 2 桁で足りる。Assist の FEATURE_STEPS、履歴のゲート、プールの幅は今の値の分布に合わせて調整されているので、定義を変えるなら全曲を同時に再計算して replay 評価で調整し直す。

---

## 3. フェーズ（A1〜A7）と状態

| ID | 内容 | 状態 |
|---|---|---|
| A1 | 誰も読まない計算と保存をやめる。rekordbox 由来の BPM が上書きで消えるバグを塞ぐ。残す出力の値は変えない | 済（release-integration） |
| A2 | 既存 DB の掃除（compaction の再構築で features_extra を許可リストで作り直し、空きブロックを回収） | 済（release-integration。実 DB での実行はオーナー） |
| A3 | rekordbox を BPM・キー・グリッドの出どころにする（保存時の 1 か所で「手動 ＞ rekordbox ＞ 自前」、既存曲の同期は計画 → 適用の 2 段、PQTZ の無い曲だけグリッド再解析） | 済。**キーの rekordbox 採用は既定 OFF**（replay 評価の合格後にオーナーが ON にする） |
| A4 | 埋め込みを任意にする（完了条件から外し、バックグラウンドで補完、欠損は再正規化、文言と「解析済み」の定義の統一） | 済 |
| A5 | Windows の整理（librosa の詳細経路と解析方法の選択を外す） | **Rust 移行に畳み込む**（Rust ワーカー 1 本にすれば自然に消える） |
| A6 | MusiCNN を Rust（ISC 版の重み）に置き換え、TF を外す | 未着手 |
| A7 | 残りの解析を Rust ワーカー 1 本にし、Essentia を撤去する | 未着手。rekordbox オプション化により範囲が広がった（下記） |

### 3.1 A7 の範囲の変更

以前の A7 は「リズムは rekordbox に無い曲だけ、キーは不要なら作らない」でした。rekordbox を持たない利用者がいるので、自前の解析一式（拍、小節頭、キー、フレーズ、ボーカル、波形、音量、キュー提案）を rekordbox 同等の合格基準で作ることが必須になりました（RB-30〜RB-36、RB-40）。A7 の評価ハーネスは RB-30 に統合し、「500 点の波形」は PDW3 の全体波形に置き換えます。Essentia を撤去する最終目標は変わりません。

---

## 4. 残りの作業項目

### 4.1 共通基盤

| ID | 内容 | 状態 |
|---|---|---|
| W1-02 | デコーダ時刻ケースのクリーンルーム仕様（LAME tag、Xing / Info、VBRI、MP4 の edts / elst、iTunSMPB、OpusHead の公開資料だけを参照）と合成フィクスチャ（既知の位置にクリック） | 仕様は `plumdeck-decode/src/spec.rs` にある。フィクスチャと crosscheck を確認 |
| W1-03 | 時間軸の ADR と Mixxx デコーダのヘッドレス計測（macOS と Windows のランナーで `waveform.requestRange{detail:'pcm'}`）、`contracts/timeline/decoder-timing-v2.json` | 未。両 OS のオフセットがケースごとに 1ms 以内で一致しないものは `unresolved` とし、Rust 解析とエンジンの切替を止める |
| W1-04 / W1-05 | `plumdeck-decode` のヘッダ解析とデコード、basis 整列、PcmPages | 済 |
| W1-06 | OS フォールバック（HE-AAC：macOS は ExtAudioFile、Windows は Media Foundation。symphonia が拒否した入力だけ）と Opus | 未（今は型付きの Unsupported） |
| W1-07 | `plumdeck-dsp`（Kaiser 窓 sinc のポリフェーズ・リサンプラと可変比モード、STFT、Hann、RBJ biquad、RMS / peak。通過域 ≤0.1 dB、阻止域 ≤−80 dB、割り当て 0） | 済（`src-tauri/crates/plumdeck-dsp`。`plumdeck-decode` の簡易リサンプラは `Resampler` トレイトの差し替えで置き換え可能） |
| W1-08 | `plumdeck-worker`（`--analysis-worker` は stdin から 1 行 16 MiB まで読み 1 op を実行して終了、`--serve` は id 付きで複数。出力は `PLUMDECK_ANALYSIS_WORKER_V1:` の後に progress か result。`catch_unwind` でパニックをエラーの result に。`priority: background` で自分の優先度を下げる） | 未 |
| W1-39 | 性能判定ハーネス（Linux で命令数の予算を判定し、壁時計は macOS で記録するだけ） | 未 |

### 4.2 解析本体

クリーンルーム：Essentia や librosa のソースは読まない。参照してよいのは論文、公開文書、リポジトリ内の自作コードだけ。Essentia / librosa は出力の比較にだけ使う。

| ID | 内容 | 受け入れ基準の要点 |
|---|---|---|
| W1-22 | リズム①：合成音源（`plumdeck-synth`：4 つ打ちの多数のテンポ、ハーフタイム、スウィング、無音イントロ、テンポの段差とランプ、揺れ、裏拍ベース、3 拍子、ノイズ）、onset（3 帯域の log1p フラックス）、tempo（自己相関と倍音和、{×½, ×1, ×2, ×⅔, ×3/2} の候補と事前分布、ヒントの BPM） | 4 つ打ちでオクターブ込み 100%、ハーフタイムはヒント無しで 90% 以上 |
| W1-23 | リズム②：グリッドの当てはめ（constant / segmented / dense）、全体の位相合わせ、整数と .5 への寄せ、`playback_grid` v3、target_basis への変換 | 4 つ打ちで ΔBPM ≤0.005（WAV）、段差は ±1 拍で分割 |
| W1-24 | キーと音色 4 種（キーはクロマとプロファイル、音色は今の定義：energy＝RMS÷0.4、noisiness＝ZCR÷0.15、brightness＝重心、danceability＝自作の DFA） | 合成 24 キーで完全一致 ≥95%、未使用の 7 出力を出さない |
| W1-25 / RB-30 | 評価ハーネス（合成、公開データセット。データはコミットしない。オーナーの rekordbox を参照にした評価はローカルのみ） | 3 系統（rust、参照、旧経路）の結果が出る |
| W1-26 | 較正（CC0 と CC-BY の曲だけで単調な写像を当てはめ）と合否ゲート | ゲート合格。満たさない項目はオーナー判断 |
| W1-27 | MusiCNN：ISC 版の重みを ONNX に変換（ReLU 前の dense 出力を埋め込みに、入力は [N,187,96] のメル）。旧モデル（NC）との重みと出力の差のレポート | レポートとモデル（ISC の LICENSE 同梱） |
| W1-28 | `plumdeck-embed`（tract）と embedding op。前段（Slaney メル 96、512/256、log10(1+10000x)）、区間は「最も音量の大きい 60 秒、hop 93」、同等性の判定（コサイン中央値 ≥0.9995、最小 ≥0.995、近傍の重なり ≥0.9 なら同じ model_name のまま） | 決定性（x86 と arm で相対 1e-5 以下）。同等でなければ新しい model_name にして全曲再計算（W1-33） |
| W1-33 | 埋め込みの全曲再計算（同等でない場合だけ）：別表に計算し、推薦は切替まで旧表だけを読み（新旧を混ぜない）、十分そろったら 1 トランザクションで入れ替える | 混ざらない、原子的、戻せる |
| RB-31〜RB-36 | 波形、テンポ・拍・小節頭、キー、フレーズ、ボーカル、キュー提案（[rekordbox-parity.md](rekordbox-parity.md)） | 同ファイル §5.3 |
| RB-40 | 自前パイプラインをバックエンドの Analyzer（S11）に接続し、合格した機能から既定にする | — |

### 4.3 完全 Rust 移行での位置づけ

- 最終形では、Rust バックエンド（[backend-port.md](backend-port.md)）の `Analyzer` トレイトが、同じバイナリの `--worker` 子プロセス（デコーダのクラッシュを隔離）を V1 で呼ぶ。Python からワーカーを呼ぶための統合作業（Python クライアント、Python 側の切替フラグ、macOS の TF 撤去を Python 上で先に行う作業）は、**S13 より前に Python 版のリリースで必要になった場合だけ**行う。
- S11 で Rust 解析がまだ合格していなければ、暫定として既存の Python 解析ワーカーを V1 で呼んで S13 に進める（オーナー判断。Python が解析ワーカーとして同梱に残る）。
- メディア処理は backend-port のスライスに入る：タグ（mutagen → lofty）は S8、詳細波形は S7（のち PDW3）、録音の書き出し（WAV / FLAC のみ）は S7 / D-04、FFmpeg CLI の撤去は Rust デコーダへの移行で行う。
- 撤去の完了条件：出荷物に Essentia、TensorFlow、librosa、onnxruntime、FFmpeg CLI、mutagen、NC のモデルが無いこと（ライセンス一覧と依存グラフで確認）。NC のモデルファイルは git rm する（履歴の書き換えはしない）。

### 4.4 DB の版

- 型の変更・削除・意味の変更なら版を上げ、追加だけなら上げない。新しい版の DB は旧ビルドが開かない（D-14 のガード）。
- 埋め込みを `FLOAT[200]` に変える案（v6）は、参照箇所が多いので見送り中。やるなら Rust 移行の後に、事前バックアップ付きの別のマイグレーションとして行う。
- timeline-v2（位置の意味が変わる）はエンジン切替の後（[engine-and-junction.md](engine-and-junction.md) W3-03）。

---

## 5. オーナー確認（README §6 に積む）

- DB のコピーで `assist_replay_eval`（emotional / bright / wordplay を含む）を回し、キーの出どころを rekordbox に切り替えてよいか判断する。
- DB のコピーで一部の曲の埋め込みを消し、replay 評価で順位に偏りが出ないことを確かめる。
- 新しい曲をドロップし、埋め込みが付く前に再生でき、後から付くことを確かめる。
- 実ライブラリでの評価レポート（PQTZ との一致、BPM の一致、音色の差、埋め込みの同等性）。数値はローカルにだけ置き、リポジトリには入れない。
- Windows 実機で、曲をドロップして時間切れにならず解析が終わること。
