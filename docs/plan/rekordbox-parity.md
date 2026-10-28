# rekordbox 同等化（rekordbox オプション）

rekordbox を持つ利用者にはそのデータを使い、持たない利用者には自前解析で rekordbox 同等の機能と見た目を提供するための計画です。拘束力のあるオーナー決定は [README.md](README.md) §2.3〜§2.7 を見てください。

- 基準：rekordbox 7.2 系のマニュアルとリリースノート、Deep Symmetry による ANLZ 形式の公開資料（<https://djl-analysis.deepsymmetry.org/rekordbox-export-analysis/anlz.html>）、実データでの検証。
- **rekordbox のデータは読むだけで、書き戻さない。**
- 公開リポジトリなので、実データから測った数値（一致率、件数など）はここに書きません。評価レポートはオーナーのローカルにだけ置きます。

---

## 0. 方針の要点

1. **3 層のデータ**：機能ごとに manual / rekordbox / own を別々に保存し、表示と演奏では「解決済みの値」を使う。優先順位は manual ＞ rekordbox ＞ own、解決は曲 × 機能の単位。rekordbox 側でフレーズ解析やキー解析を OFF にしている利用者もいるので、「グリッドは rekordbox、フレーズとキーは自前」のような混在が普通に起きる前提で作る。
2. **rekordbox の時刻は rekordbox 上の値のまま持つ**。再生の時間軸への変換は読み出すときに行う（今の Python は取り込み時に補正済みの値を `cue_points_json` に書き、どの補正を使ったか記録していない。この方式はやめる）。
3. **rekordbox の読み取りは自作の `plumdeck-rekordbox`**（実装済み）。GPL のコード（Mixxx の rekordbox 取り込み、rbox）は読まない・使わない。rekordcrate（MPL-2.0）も、PWV6 / PWV7 に未対応で assert が厳しいので使わない。
4. **自前解析は、rekordbox を持たない人にとっての主経路**。フォールバックではなく、rekordbox 同等の合格基準を課す。評価の参照にはオーナーの rekordbox データを使い、rekordbox の誤りまで真似しないよう人の裁定を併用する。
5. **波形は 3 バンドの形式 1 つ（PDW3）にまとめる**。PWV6 / PWV7 からも自前生成からも同じ形で供給し、描画部品も 1 つにする。
6. **キーは内部で KeyId（0〜23）に統一**し、表記は表示時だけ変換する。相性は「実効キー」（キーシフトと、キーロック OFF でテンポを変えた分を反映）で判定する。
7. **キー相性の推薦フィルタ**（UI 名は Alphanumeric。既定 OFF）は決まった規則で候補を絞る。OFF のときは今の順位を 1 つも変えない。スコアの重みも変えない。

---

## 1. GAP マトリクス

記号：✅ 実装済み、◐ 一部、✕ 未実装、? 要確認。優先度：**P0**＝rekordbox 同等の中核（解析結果、その表示、キー表記と相性、確定済みバグ）、**P1**＝P0 の後、**P2**＝演奏機能の拡張や細かな UI、**外**＝範囲外（[README.md](README.md) §2.9）。

### 1.1 解析と表示

| 項目 | plumdeck の現状 | rekordbox ありのデータ源 | 自前の手段 | 優先度 |
|---|---|---|---|---|
| BPM | ✅ rekordbox を優先（release-integration）。自前は Essentia（AGPL のため出荷できない） | djmdContent.BPM（100 倍の整数）、PQTZ | 自前 v1（自己相関と倍音和、4:3・3:2 の誤り対策、整数と .5 への寄せ） | P0 |
| 拍グリッド | ✅ PQTZ、無ければ自前 | PQTZ | 自前 v1（一定 / 区間 / 拍ごとの DP） | P0 |
| 小節頭 | ◐ rekordbox 由来のグリッドだけ。自前は推定しない | PQTZ の beat_number | 自前 v1（拍同期の特徴量で 4 択、フレーズと同時最適化） | P0 |
| BPM / Grid モードと範囲 | ◐ 一定か可変かは自動、範囲指定なし | 結果を読むだけ | 同じ選択肢を自前解析に | P1 |
| グリッド編集 | ✅ ほぼ同等（BeatGridEditor）。メトロノームなし | 編集は manual 層に | 同じ | P1 |
| Analysis Lock | ✕ | 読まない | manual 層にロックの印 | P1 |
| 解析項目の選択 | ✕ | — | 処理ごとの ON/OFF | P1 |
| キー検出 | ◐ 自前（Essentia）。品質に課題 | djmdKey.ScaleName（意味の無い値も混じる） | 自前 v1（HPSS、チューニング推定、倍音抑制、テンプレート） | P0 |
| フレーズ解析 | ✕ | PSSI と PQTZ | 自前 v1（小節単位の DP、mood 分類、HMM ラベル） | P0 |
| フレーズ表示 | ✕ | PSSI | 自前の結果 | P0 |
| PHRASE EDIT | ✕ | — | manual 層 | P1 |
| ボーカル位置 | ✕ | 形式不明のため当面読まない | 自前 v1（調波成分、YIN の有声度） | P1 |
| 3 バンド波形 | ◐ 自前の 3 帯域の計算が複数あり、描き方も一致しない | PWV7（150 列/秒）、PWV6（1200 列）。バイト順は低・中・高 | 自前生成を PWV7 に当てはめる | P0 |
| RGB / BLUE 波形 | ✕ | RGB は PWV5 / PWV4、BLUE は PWV3 / PWAV | 自前の 3 帯域から作る | P1 |
| 一覧のプレビュー波形 | ◐ 行ごとに全曲デコードしていて重い | PWV6 | 解析時に全体波形を保存 | P0 |
| Auto Cue | ✕ | 保存されない | 無音のしきい値 | P1 |
| 1.1Bars の Memory Cue | ✕ | djmdCue のコメント「1.1Bars」 | グリッドから決まる | P1 |
| キューの自動生成 | ✕ | 結果は djmdCue から | フレーズと手置きキューの傾向から提案 | P1 |
| Auto Gain | ✕ | djmdMixerParam（float32、−10 LUFS に合わせるゲインと推定） | BS.1770 を自前実装 | P1 |
| STEMS | ✕ | — | 後で代替手段（README §2.9） | 後 |

### 1.2 キーの表示と相性（重点）

| 項目 | plumdeck の現状 | 方針 | 優先度 |
|---|---|---|---|
| キー表記の切り替え | ✕ 生の文字列（「Ab minor」など） | 既定 Alphanumeric（8A）、Classic に切替。Open Key とタグのままは任意 | P0 |
| デッキのキー表示 | ◐ release-integration で KEY SHIFT と今のキーを表示 | 元のキー、今のキー、差（±N）。±30 セント超は「≈」 | P0 |
| Traffic Light | ✕ | 既定 ON。ブラウザのキー列を基準デッキ（MASTER か各デッキ）との関係で色分け | P0 |
| 推薦と候補カードの相性表示 | ◐ Setlist 自動生成だけ。変換に部分一致のバグ（`src/components/setlist-creator/utils.ts`） | 同じ分類器を全画面で使う | P0 |
| キー相性の推薦フィルタ | ✕（判定表は `backend/utils/audio_math.py` にある） | 既定 OFF。ON は same / ±1 / 平行調だけ、同じキーを先頭、キー不明は最後 | P0 |
| キーのフィルタ（KEY / BPM） | ◐ BPM の幅はある。キー絞り込みの大小文字バグは修正済み | マスター連動のフィルタ | P1 |
| KEY SHIFT / SYNC / RESET | ✅ エンジンとデッキ UI（release-integration） | — | 済 |
| Matching | ✕ | djmdRecommendLike と plumdeck での登録 | P2 |

### 1.3 キューとループ

| 項目 | plumdeck の現状 | rekordbox のデータ源 | 優先度 |
|---|---|---|---|
| Hot Cue A〜P | ◐ A〜H の位置とコメントだけ取り込み、色は捨てる | djmdCue.Kind（1,2,3,5〜9 が A〜H、10〜17 が I〜P） | P0 |
| Hot Loop / Active Loop | ◐ 保存ループはある。rekordbox のループ終点は読んでいない | OutMsec、BeatLoopSize、ActiveLoop | P0 |
| Memory Cue / Loop | ◐ パッドで呼び出し・保存・削除できるが保存先はブラウザ側。波形に出ない | djmdCue.Kind=0 | P0（DB へ移す、オーナー決定） |
| キューの色と HOT CUE 色モード | ◐ 波形とパッドで色が食い違う（要確認） | ColorTableIndex、Color、HotCueColorType | P1（食い違いは P0） |
| キューのコメント | ✅ ラベルとして取り込む | Comment | P0（自動の「1.1Bars」の見せ方を決める） |
| 拍カウント（小節数、次の Memory Cue まで） | ✕ | PQTZ と Memory Cue | P1 |
| Memory → Hot 変換、Hot Cue Bank、Smart Cue、Gate Cue など | ✕ / ? | — | P2（エンジン） |

### 1.4 ライブラリとブラウザ

| 項目 | plumdeck の現状 | 方針 | 優先度 |
|---|---|---|---|
| 取り込み | ◐ ドロップと Explorer | Explorer を廃止し、ドラッグ＆ドロップ・「曲を追加」・rekordbox コレクション取り込みの 3 経路と 1 つのキュー（README §2.5） | P0 |
| 対応フォーマット | ✅（Mixxx でデコード） | Rust デコーダ（symphonia。HE-AAC は OS デコーダ） | P0 |
| タグ編集とアートワーク | ✅ | mutagen（GPL）を lofty に | P0 |
| My Tag / Rating / Color / Comment | ✕ | rekordbox から読み、編集は manual 層 | P1 |
| プレイリストのフォルダ | ◐ 階層なし。rekordbox のプレイリストは読める | フォルダ対応 | P1 |
| Intelligent Playlist | ✕ | 自前のルールエンジン、rekordbox の SmartList の読み取り | P1 |
| RELATED TRACKS / Track Suggestion | ◐ 推薦と Assist | 条件の追加 | P1 / P2 |
| 再生履歴 | ✅（Assist で使用） | — | 済 |
| rekordbox XML 書き出し | ✅ グリッドの出力は修正済み。キュー色と時刻補正の戻しは要確認 | — | P0 |
| CDJ 用 USB（export.pdb）作成、PRO DJ LINK、Tag List、クラウド、ストリーミング | ✕ | 範囲外 | 外 |

### 1.5 演奏

Play モード（デッキ、パッド、FX、ミキサー、サンプラー、録音）は [engine-and-junction.md](engine-and-junction.md) で扱います。rekordbox 由来のデータが必要な演奏機能は次の 3 つです：拍と小節頭まで合わせる BEAT SYNC（RB-41）、新しいパッドモード（RB-42）、Automix と MIX POINT LINK（RB-46）。

### 1.6 確定済みのバグ（RB-01 の残り）

| バグ | 場所 | 状態 |
|---|---|---|
| 部分一致の変換で「Ab minor」が 10A になる | `src/components/setlist-creator/utils.ts` | 未修正。生成した表（RB-02）か完全一致に |
| キーのフィルタが 0 件 | `backend/infra/repositories/track_repository.py` | 修正済み（大文字小文字を無視） |
| XML 書き出しでグリッドが出ない | `backend/app/services/workflow_service.py` | 修正済み（release-integration） |
| デッキ見出しのキー UI が無効 | `src/components/play/SoftwareDeck.tsx` | 修正済み（release-integration） |
| HOT CUE マーカーの色がパッドと違う | `src/components/play/DeckWaveform.tsx` | 要確認 |

---

## 2. データモデル

### 2.1 層と解決

```rust
enum Source { Manual, Rekordbox, Own }
struct Rev<T> { value: T, revision: String, updated_at: Timestamp, confidence: Option<f32> }
struct Layered<T> { manual: Option<ManualEntry<T>>, rekordbox: Option<Rev<T>>, own: Option<Rev<T>> }
enum ManualEntry<T> { Value(Rev<T>), Pin(Source) }   // Pin = 「この曲はこの層を使う」
struct Resolved<T> { value: T, source: Source, available: Vec<Source>, stale: bool }
```

- 解決順：manual の Value → manual の Pin が指す層 → rekordbox → own。
- 全体設定：機能ごとに「rekordbox の値を使う」（既定 ON。キーだけは replay 評価の合格まで既定 OFF）。
- 曲ごとの操作：「rekordbox に戻す」（manual を消す）、「自前を使う」（Pin(Own)）、「自前で再解析」。
- stale：フレーズの基になったグリッドが今のグリッドと食い違うと true（§5.3）。
- 削除の印（tombstone）：plumdeck で消した rekordbox のキューが次の同期で復活しないよう、`(track, feature, item_key)` を manual 層に記録する。

### 2.2 機能ごとの値

| 機能 | 値の形 | manual | rekordbox | own | 解決の単位 |
|---|---|---|---|---|---|
| BPM・グリッド・小節頭 | `Grid{kind: Constant/Segmented/Dense, beats:[(t_ms, beat_no 1–4)], bpm}` | BeatGridEditor（既存の `beat_grid_json`） | PQTZ | 自前 v1 | 曲ごとに丸ごと（層を混ぜない） |
| キー | `KeyEst{key: KeyId, confidence, second: Option<KeyId>}` | 手入力 | ScaleName を正規化 | 自前 v1 | 曲ごと |
| フレーズ | `PhraseSet` | PHRASE EDIT | PSSI | 自前 v1 | 曲ごと（グリッドとの整合を確認） |
| キュー・ループ | `CueEntry{kind: Hot(0–15)/Memory, start, end?, beat_loop?, color, comment, active_loop}` | plumdeck での編集 | djmdCue | 提案のみ（自動では書かない） | Hot はスロット、Memory は項目ごと |
| 波形 | `PDW3` | — | PWV7 / PWV6（ほか PWV5/4/3、PWAV） | 自前生成 | 曲ごと |
| ボーカル | `VocalRegions{bars, confidence}` | 修正（任意） | 当面なし | 自前 v1 | 曲ごと |
| Auto Gain | `gain_db` | 曲ごとの trim | djmdMixerParam | BS.1770、−10 LUFS 基準 | 曲ごと |
| Rating / Color / Comment / MyTag | 各値 | plumdeck での編集 | djmdContent、djmdSongMyTag | — | 項目ごと |
| Matching・履歴 | ペア、行 | plumdeck での登録 | djmdRecommendLike、djmdHistory | — | 和集合 |
| Analysis Lock | bool | manual のみ | 読まない | — | 曲ごと |

**ジャンルはこの仕組みに入れない**：rekordbox やタグのジャンルは使わず、plumdeck の検証済みジャンルと端末上で学習する分類器だけを使う（README §2.4）。

### 2.3 保存場所（追加のみ）

| テーブル / キャッシュ | 内容 |
|---|---|
| `rb_link` | 曲と rekordbox の対応（`track_id`、`rb_content_id`、ANLZ の各パス、ファイルの size と mtime、`rb_local_usn`、`analysis_updated`、`cue_updated`、`timebase_case`、`synced_at`） |
| `rb_layer` | rekordbox から読んだ値を rekordbox 上の時刻のまま（`bpm_x100`、`key_raw`、`key_id`、`grid_raw`、`phrase_raw`、`cues_raw`、`gain_db`、`rating`、`color_id`、`comment`、`mytags`、`overview_pwv6`、`revision`） |
| `own_layer` | 自前解析の結果（`track_id, feature, version, value, confidence, timeline_basis, computed_at`）。`track_analyses` とは別の表 |
| `manual_override`、`manual_tombstone` | 手動の値、Pin、削除の印。既存の `track_performance_metadata` は manual 層としてそのまま使う |
| tracks の追加列 | `key_id SMALLINT`、`bpm_source`、`key_source`、`grid_source`、`phrase_source`。既存の `bpm` と `key` も解決済みの値で埋める |
| `waveform-v3/<hash>.pdw3` | 全体波形と詳細波形のファイルキャッシュ（DuckDB には入れない） |

- PWV7 は曲ごとに大きいので `rb_layer` に入れず、ANLZ から必要なときに読んで LRU で持つ。
- スキーマ版は DB の版の台帳と同じ規則で採番する（型の変更・削除・意味の変更なら版を上げ、追加だけなら上げない）。

### 2.4 時間軸

- rekordbox の時刻 → 再生の時刻：ファイルごとに `timebase_case` を判定（MP3 のヘッダの種別 A〜D、AAC、WAV、FLAC、AIFF。判定は `plumdeck-decode` のクリーンルーム実装）→ 実測の表 `decoder-timing-v2.json`（RB-12）からずれ Δrb を引く → `t_canonical = t_rb − Δrb` → エンジン切替前は legacy の基準にさらに変換。
- フレーズ：PSSI の `beat` は「PQTZ の 1 始まりの拍番号」。補正前の PQTZ で ms に直してから補正する。今の Python の `adjust_grid` は補正値より前の拍を削除して番号がずれるので、同じことをしない。
- 波形：PWV7 は `round(Δrb × 0.15)` 列ずらす。
- manual 層と own 層は `timeline_basis` を付けて保存する。timeline-v2 への移行（W3-03）でずらすのはこの 2 層だけ。

### 2.5 変更検知と再同期

- 大まか：master.db と WAL の mtime。細かく：`rb_local_usn`、`AnalysisUpdated`、`CueUpdated` が変わった行だけ読み直す。ANLZ はファイルの mtime と size。
- 同期のたびに `rb_layer` を差分更新し、影響を受けた曲だけ解決器で tracks の追加列を書き直す。

### 2.6 rekordbox が無い、または読めない場合

リーダは `Unavailable{reason}` を返し、すべての機能は own 層で動く。UI では rekordbox 専用の設定を隠し、「rekordbox: 未接続」と表示する。master.db の場所は設定と `PLUMDECK_REKORDBOX_DB` で上書きできる。

### 2.7 API と UI

- `GET /api/tracks/{id}/analysis-view`：機能ごとに `{value, source, available, stale}`。
- `GET /api/tracks/{id}/waveform?level=overview|detail|fine&mode=3band|rgb|blue`：PDW3 のバイナリ。
- 各機能の横に小さな出どころバッジ（RB / 自前 / 手動）。メニューから「rekordbox に戻す」「自前を使う」「再解析」。

---

## 3. rekordbox リーダ（`src-tauri/crates/plumdeck-rekordbox`、実装済み）

- **master.db**：rusqlite の `bundled-sqlcipher` で読み取り専用に開き、パスフレーズを設定してから `PRAGMA query_only=ON`。鍵は pyrekordbox（MIT）と同じ手順で導出し（MIT の表示は THIRD_PARTY_NOTICES に）、env `PLUMDECK_REKORDBOX_KEY` で上書きできる。鍵は Debug 表示でも伏せ、エラー文に出さない。鍵違い・ファイルなし・別の DB は `Unavailable`（panic しない）。
- 読むテーブル：djmdContent、djmdKey、djmdCue、djmdMixerParam、djmdMyTag / djmdSongMyTag / djmdColor、djmdPlaylist / djmdSongPlaylist、djmdHistory / djmdSongHistory、djmdRecommendLike、djmdRelatedTracks、contentFile、djmdProperty。削除済みの行は返さない。
- **`agentRegistry` は `registry_id` を指定した SELECT だけ**（同じ表に認証情報があるため）。ソースを走査するテストで固定済み。
- 確定した値の意味：Kind 1,2,3,5〜9＝Hot Cue A〜H、10〜17＝I〜P、0＝Memory Cue、OutMsec=−1＝ループでない、BeatLoopSize＝`(分子 << 16) | 分母`、Color=255 かつ ColorTableIndex=0＝既定色、MixerParam＝`(High << 16) + Low` を float32 として読み 20·log10 で dB。
- ScaleName のゴミ（URL など）は None。Alphanumeric 表記で入っている値は KeyId に変換。DBVersion が想定外なら警告して読み続ける。
- **ANLZ**：`PMAI` で始まり、各タグが `fourcc / len_header / len_tag`（big-endian）。長さを境界検査し、1 ファイル 64MB まで。未知のタグ（PQT2、.3EX）は読み飛ばし、想定外の値はそのタグだけを無効にする。パスは `contentFile.Path`、無ければ `AnalysisDataPath` に share ルートを足す。ローカルの解析ファイルの PPTH は「?/<ファイル名>」だけなので、それで取り違えを検出する。
- 各タグ：PQTZ（beat_number u16、tempo u16 = BPM×100、time u32 ms）、PWV7 / PWV6（1 列 3 バイト、低・中・高、0〜127）、PWV5（RGB 各 3bit と高さ）、PWV4、PWV3 / PWAV（高さ 5bit、白さ 3bit）、PWVC（保持のみ）、PCOB / PCO2（USB エクスポート用）、PSSI（mood、end_beat、bank、24 バイトのエントリ、XOR マスクの判定と解除。end_beat が PQTZ の拍数を超えるものは切り詰める）。
- 一覧で使うのは PWV6 だけ。PWV7 はデッキにロードしたときに読む。
- 残り：`key.rs`（`plumdeck-key` を使う）、`gain.rs`、`timebase.rs`（RB-12）、`sync.rs`（差分同期の材料）、`export_xml.rs`（既存の XML 書き出しの移植）。
- CI では合成した ANLZ と DB だけを使う。実データのテストは `#[ignore]` で、オーナーのローカルでのみ。

---

## 4. 自前解析パイプライン（Rust）

```
plumdeck-decode（symphonia、gapless は自前で処理。HE-AAC は OS デコーダ）
  → plumdeck-dsp（STFT、biquad、リサンプラ）
  → plumdeck-analysis
      features：帯域別フラックス / クロマ / log-mel / 3 バンド包絡（1 回のデコードを共有）
      ops：grid（拍と小節頭）→ key → loudness → waveform → vocal → phrase → cue_suggest → autocue
  → plumdeck-worker（V1 プロトコル）→ バックエンドの Analyzer トレイト（S11）
```

結果は `own_layer` に版と `timeline_basis` を付けて保存。解析設定（RB-37）で処理ごとの ON/OFF と BPM 範囲を指定できる。

### 4.1 v1 の方式

| 機能 | 方式 |
|---|---|
| テンポ・拍 | 自己相関と倍音和でテンポ候補。BPM 分布の事前分布と 4:3・3:2 の誤り対策（キック帯域の周期性、スネアが 2・4 拍目に来るか）。一定テンポは 0.001BPM 刻みで最大化して整数と .5 に寄せる。位相はキックで重み付けし裏拍の誤りを防ぐ。可変テンポは区間か拍ごとの DP |
| 小節頭 | 曲ごとの 4 択。和声の変化、ベースの立ち上がり、キックとスネアの位置、4・8 小節周期のエネルギー、無音明けの拍をロジスティック回帰で組み合わせ、フレーズ境界と同時に最適化 |
| キー | HPSS で打楽器成分を除き、倍音を抑え、チューニングを推定。高音と低音のクロマを 24 キーのテンプレートと照合。確度と第 2 候補も出す |
| フレーズ | 小節ごとの特徴（3 帯域のエネルギー、帯域別オンセット率、クロマ、音色、ボーカル確率）。自己類似行列の新規性で境界候補、長さの事前分布と 4・8 小節の位相を入れた半マルコフ DP で区切る。mood はロジスティック回帰、ラベルは HMM、Fill は境界の手前で判定 |
| ボーカル | 調波成分に YIN の有声度、ビブラート、平坦度。小節ごとにロジスティック回帰、ヒステリシスで区間に |
| 波形 | LR4 クロスオーバー、包絡（RMS かピーク）、`h=clip(round(g·env^γ),0,127)` |
| 音量 | BS.1770-4 の積分ラウドネス。`gain_db = −10 − LUFS` |
| キュー提案 | 最初の拍・小節頭・音の出だし、フレーズから決まる点（最初の Chorus、Outro の開始、終わりから 16 / 32 小節前など）、ボーカルの始まりと終わり、Intro / Outro のループ |

### 4.2 学習するモデルについて（オーナー決定）

- v1 は DSP と少数のスカラーの較正にとどめる。
- 学習するモデル（v2、RB-44）は、**各利用者の PC 上で、その利用者自身の検証済みラベルから学習する**。オーナーの商用音源で学習した重みは出荷しない。推論は tract。
- ボーカルの正解データは、まず同期歌詞（LRCLIB）を弱いラベルにし、後でオーナーが少数の曲に手で注釈を付ける。

---

## 5. 評価（`tools/analysis-eval`、出荷しない）

### 5.1 参照データと分割

- 参照データはオーナーのローカルで、読み取り専用コピーから作る（曲ごとに ID、コーデック、timebase_case、長さ、BPM、キーと出どころ、PQTZ、PSSI、PWV6 / PWV7、MixerParam、キュー、ボーカル区間の弱ラベル）。**リポジトリにもクラウドにも置かない。**
- 「アーティスト＋タイトル」を正規化してまとめ、ハッシュで固定して開発 / 較正 / テストに分ける（同じ曲の別エディットが分割をまたがないように）。テスト分割は合否の判定にだけ使う。
- クラウドのセッションでは、合成音源と許諾の明確な公開データセットで開発し、実データでの判定はオーナー確認に積む。

### 5.2 評価の設計上の注意

- キュー：自動で付いた「1.1Bars」のキューを参照から外し、「1.1Bars の再現」と「手で置いたキューの予測」を別々に評価する。
- キー：参照を 3 層（rekordbox とタグが一致する合意セット、rekordbox にだけある曲、人の裁定セット）に分ける。「rekordbox との一致率」は参考値。
- 相性：テンポを合わせた後の「実際に鳴るキー」も評価する。
- フレーズ：フレーズの開始が PQTZ の小節頭に乗らない曲（解析後にグリッドを直した曲）は参照から外す。
- 条件をそろえる：自前解析も rekordbox と同じ BPM 範囲とモードで動かす。テンポが変化する曲は別に集計する。
- 裁定は 1 項目 300 件程度で、Wilson の 95% 区間の上限で判定する。
- 再現性：macOS と Windows、複数回の実行で結果が同じか照合する。

### 5.3 合格基準（テスト分割、時間軸の補正後）

v1 の数値は暫定。開発分割で一度走らせた後に 1 回だけ見直し、テスト分割を見て変えない。

| 項目 | 指標 | v1 | v2 |
|---|---|---|---|
| テンポ | 一定テンポの曲で BPM が ±0.05 以内 | ≥90% | ≥94% |
| | ±4% 以内（Acc1） | ≥97% | ≥98.5% |
| | 拍子レベルの誤り（2 倍、半分、3:2、4:3） | ≤3% | ≤1.5% |
| 拍 | 裏拍の誤り | ≤4% | ≤2% |
| | ±15ms の F 値 0.9 以上の曲の割合 | ≥90% | ≥95% |
| | 位相の残差 p50 / p90 | ≤3ms / ≤10ms | ≤2ms / ≤8ms |
| | テンポ変化曲の F 値中央値（±15ms / ±70ms） | ≥0.80 / ≥0.95 | ≥0.88 / ≥0.97 |
| 小節頭 | 4 択の正解率（拍が合っている曲） | ≥88% | ≥94% |
| キー | 合意セットでの完全一致 | ≥85% | ≥90% |
| | 裁定後の誤り率（Wilson の上限） | ≤8% | ≤5% |
| | rekordbox との MIREX 加重スコア（参考） | ≥0.78 | ≥0.83 |
| | 相性フィルタの 2 曲判定が rekordbox のキー同士の判定と一致 | ≥0.88 | ≥0.91 |
| フレーズ | 境界の F 値（±1 小節） | ≥0.75 | ≥0.85 |
| | 境界の F 値（同じ小節） | ≥0.60 | ≥0.72 |
| | mood の均衡正解率 | ≥0.75 | ≥0.85 |
| | 拍単位のラベル正解率 | ≥0.70 | ≥0.80 |
| | Intro の終わり / Outro の始まりが ±1 小節 | ≥85% | ≥92% |
| ボーカル | 小節単位の F1 | ≥0.80 | ≥0.90 |
| | イントロ・アウトロの「ボーカルなし」の適合率 | ≥0.95 | ≥0.97 |
| 波形 | 帯域ごとの Pearson 相関の中央値（低・中 / 高） | ≥0.90 / ≥0.85 | — |
| | オーナーが見比べて「同等か許容」 | ≥80% | — |
| Auto Gain | rekordbox との差 ±0.5dB 以内 | ≥95% | — |
| キュー | 1.1Bars の再現 | ≥99% | — |
| | 手置きの A が 1 位 / 上位 3 件（±1 拍） | ≥40% / ≥65% | ≥50% / ≥75% |
| | 全ホットキューの再現率（候補 8 件まで） | ≥60% | ≥70% |
| 時間軸 | 補正後の PQTZ と自前の拍の差の中央値（種別ごと） | ≤3ms | — |
| | 補正後の PWV7 のずれ | ≤1 列 | — |
| 性能 | 4 分の音源を 1 コアで（埋め込みを除く全解析） | ≤1.0 秒 | ≤2.0 秒 |

**UI の合格**：オーナーの rekordbox の画面と並べて比べる（フレーズの色と Fill、名前の常時 / ホバー表示、キー表記、Traffic Light の色と範囲、3Band / RGB / BLUE の配色、HOT CUE 色の 4 モード）。

---

## 6. 波形モデル（PDW3）

```
PDW3 {
  magic "PDW3", version, source(Rekordbox|Own), timeline_basis, offset_cols(i16)
  band_order = [Low, Mid, High]（固定）、scale = "rb127"（0–127）
  overview: 1200 列 × 3B        … PWV6 と同じ意味。一覧のプレビューとデッキの全体波形
  detail:   150 列/秒 × 3B      … PWV7 と同じ意味。拡大波形
  fine?:    約 690 列/秒 × 3B   … 自前のみ。グリッド編集で更に拡大するとき
  rgb?: PWV5 / PWV4 相当, blue?: PWV3 / PWAV 相当, norm?: 帯域ごとの表示スケール
}
```

- rekordbox の曲：PWV6 / PWV7 を時間軸の分だけずらして詰め直す。fine は必要なときに自前で作る。
- rekordbox に無い曲：解析時に自前で全レベルを作る（4 分の音源で 50ms 未満が目標）。ロード直後の未解析の曲でも、ワーカーがデコードと同時に作る（0.5 秒以内が目標）。
- 置き換えるもの：500 点の概形（互換のため S14 まで残す）、FFmpeg での詳細波形、エンジン側の波形 Pyramid（パリティのため切替まで残し、その後 RB-43 で PDW3 に）。
- 描画：WebGL2（無ければ Canvas2D）の部品 1 つを、デッキの全体波形・拡大波形・一覧のプレビュー・ミニプレーヤーで共用。3Band は低域（濃い青）と中域（アンバー）を重ね、重なりは茶、高域（白）を最後に。全体波形は 3 帯域を積み上げる。色の値は自前のトークン（意味だけ rekordbox に合わせる）。
- 重ねて描くもの：グリッド線（小節頭は赤）、キュー（パッドと同じ色）、Memory Cue、ループ、フレーズの帯、ボーカルの帯、再生済み部分の減光。

---

## 7. フレーズ

```rust
struct PhraseSet {
  source: Source, grid_ref: GridRef { source: Source, revision: String, n_beats: u32 },
  mood: Mood /* High | Mid | Low */, bank: u8 /* 保持のみ */, end_beat: u32,
  phrases: Vec<Phrase>,
}
struct Phrase {
  start_beat: u32,              // grid_ref の 1 始まりの拍番号（PSSI と同じ）
  kind: PhraseKind,             // Intro | Up | Down | Chorus | Verse | Bridge | Outro
  variant: u8,                  // Intro 1/2, Up 1/2/3, Chorus 1/2, Outro 1/2, Verse 1–6
  fill_start_beat: Option<u32>, // Fill-in（最大 4 拍）
  extra_beats: [Option<u32>; 3],
}
```

| mood | PSSI の kind → 名前 |
|---|---|
| Low | 1 Intro、2〜4 Verse 1、5〜7 Verse 2、8 Bridge、9 Chorus、10 Outro |
| Mid | 1 Intro、2〜7 Verse 1〜6、8 Bridge、9 Chorus、10 Outro |
| High | 1 Intro（k1 で 1 か 2）、2 Up（k2 と k3 で 1 / 2 / 3）、3 Down、5 Chorus（k1 で 1 か 2）、6 Outro（k1 で 1 か 2） |

- 表示：全体波形と拡大波形の下に種類ごとの色の帯、Fill は斜線、名前は「常に表示 / ホバー時だけ」を設定で。plumdeck 独自の追加（OFF にできる）として、デッキ見出しに今のフレーズ名と次の境界までの小節数、Assist と Junction に「次の境界まで N 小節」を渡す。
- 編集（RB-38）：境界のドラッグ（越えると結合）、CUT、種類の変更、CLEAR。manual 層に保存し、rekordbox には書かない。
- グリッドとの整合：`grid_ref` と今のグリッドが違えば、開始拍を時刻に直して最も近い小節頭に合わせる。半分以上が乗らなければ `stale` にし、自前解析が有効なら作り直す。UI に「グリッドの変更でフレーズが古くなっています」と再解析ボタン。

---

## 8. キー（`plumdeck-key`、RB-02）

- `KeyId = pc + 12·minor`（0〜23）。
- Classic：長調 C, Db, D, Eb, E, F, F#, G, Ab, A, Bb, B／短調 Cm, Dbm, … , Bbm, Bm（rekordbox と同じ綴り）。
- Alphanumeric（1A〜12B、**既定**）：番号は `((7·pc_M) mod 12 + 7) mod 12 + 1`（pc_M は長調なら主音、短調なら平行長調の主音）、長調 B / 短調 A。Abm=1A、B=1B、Ebm=2A、F#=2B、Am=8A。
- Open Key（任意）：`((n − 8) mod 12) + 1` に d / m。タグのまま（生の文字列）も選べる。
- 入力として受け付けるもの：「A minor」「Am」「a min」「8A」「1m」「G#m」「A#m」（→ Bbm）、♯ と ♭。それ以外は None。
- 対応表が今複数か所（`backend/utils/audio_math.py`、`backend/domain/services/assist_recommendation.py`、`src/components/setlist-creator/utils.ts`、`src/components/play/track-sort.ts`、`src/components/music-library/constants.ts`、`src/services/dj-engine/musical-key.ts`）にあるので、Rust の 1 か所にまとめ、`contracts/key/keys.json` を生成してフロントが読む。移行が終わるまで Python の `normalize_key` と全件一致をベクタで保証する。
- **実効キー**：`semitones = key_shift + (keylock ? 0 : 12·log2(tempo_ratio))`、`effective = rotate(base, round(semitones))`。デッキ見出しに元のキー・今のキー・差。|cents| > 30 で「≈」。候補曲の判定は既定で「キーロック ON」とみなし、設定で「テンポを合わせた後のキー」に切り替えられる。
- **マスターデッキ（相性の基準）**：シンクの leader → 音が出ているデッキのうち最も早く再生を始めたもの → 最後にロードしたデッキ。Junction デッキは相手からキーが届いているときだけ候補。Assist（rekordbox を演奏アプリとして使う場合）は rekordbox の画面のマスターとそのキー。ブラウザの見出しで MASTER（既定）/ A〜D を選べる。
- **関係の分類**：same、adjacent（同じ文字で ±1）、relative（同じ番号で文字違い）、diagonal、plus2、semitone、other。Traffic Light の段階は L1＝same / adjacent / relative（緑、既定）、L2＝diagonal / plus2（黄、設定で ON）。キー不明は灰色の「—」、確度の低い自前のキーは「?」。
- **表示場所**：Play の一覧と Library のキー列（セルの背景色、見出しで基準デッキ）、デッキの見出し、Assist の候補カードと Play の推薦パネル（「同じキー」「+1」「平行調」などのラベル）、Setlist 自動生成の ◎○△、MCP の出力（`key`、`key_id`、`key_relation`）。
- **推薦のキー相性フィルタ**：Assist はプリセットと別のチェックボックス（transition と同じ並び）、Play の推薦パネルは切り替えボタン、Setlist の Recommend / Auto、MCP の推薦系と set planning のツールに `camelot: bool`（引数名は互換のため。UI 表記は Alphanumeric）。既定 OFF、選択は保存。ON のとき same / adjacent / relative だけ残し、同じキーを先頭、キー不明は最後。確度の低い自前のキーは第 1 候補だけで判定。スコアの重みは変えない。

---

## 9. 作業項目（RB-xx）

| ID | 内容 | 依存 | 規模 | 状態 |
|---|---|---|---|---|
| RB-01 | [P0] 確定済みバグ（§1.6） | A1 | S | 一部済 |
| RB-02 | [P0] `plumdeck-key` と `contracts/key/keys.json`、TS モジュールの生成 | S0 | S | |
| RB-03 | [P0] キー表記の設定と全画面の統一（既定 Alphanumeric） | RB-01, RB-02 | S〜M | |
| RB-04 | [P0] マスターデッキの決定と実効キー、デッキ見出しの表示 | RB-01, RB-02 | M | |
| RB-05 | [P0] Traffic Light（既定 ON）と全画面の相性表示 | RB-03, RB-04 | M | |
| RB-06 | [P0] 推薦のキー相性フィルタ（既定 OFF、キー不明は最後） | RB-02 | S〜M | |
| RB-07 | [P0] rekordbox キーの採用（`key_id` と `*_source` 列、ゴミの除外、計画→適用の 2 段の同期）。既定 ON への切替は replay 評価の合格後 | A3, RB-02 | S | 一部済 |
| RB-10 | [P0] master.db リーダ | S0 | M | 済 |
| RB-11 | [P0] ANLZ パーサ | S0 | M | 済 |
| RB-12 | [P0] 時間軸の補正表 v2（MP3 の A〜D、AAC、ALAC、FLAC、WAV、AIFF。PQTZ とオンセット、PWV7 との相互相関。LAME の公開仕様によるクリーンルーム） | W1-02, W1-03, RB-11 | M | |
| RB-13 | [P0] 3 層のデータモデルと解決器（Rust）、差分同期、`analysis-view` と `waveform` のエンドポイント | RB-10/11/12, RB-02, S5 | L | |
| RB-14 | [P0] 先に見せるための Tauri コマンド（rekordbox 層のフレーズ、PDW3、キューと色、キー）。DuckDB の書込み権と競合しない。S13 で API に統合 | RB-10/11/12 | S | |
| RB-15 | [P0] キューとループの完全取り込み（Hot Cue I〜P、Memory Cue の DB 化と既存の保存先からの移行、ホットループ、色、1.1Bars、XML の色と時刻補正の戻し） | RB-01 | M | |
| RB-20 | [P0] PDW3 と描画部品の統一 | RB-14 | L | |
| RB-21 | [P0] フレーズ表示 | RB-14, RB-20 | M | |
| RB-30 | [P0] 評価ハーネス（W1-25 と統合） | W1-25, RB-10/11/12 | M | |
| RB-31 | [P0] 自前の 3 バンド波形を PWV7 に当てはめる。RGB / BLUE の派生 | RB-30, W1-05, W1-07 | S〜M | |
| RB-32 | [P0] テンポ・拍・小節頭 v1 | RB-30, W1-22 | L | |
| RB-33 | [P0] キー v1（rekordbox が無い利用者のために必須） | RB-30, W1-07 | M〜L | |
| RB-34 | [P0] フレーズ v1 | RB-32, RB-30 | L | |
| RB-40 | [P0] 自前パイプラインを S11 の Analyzer に接続し A7 を置き換える（合格した機能から既定に） | RB-13, RB-31〜34, S11 | M | |
| RB-22 | [P1] Auto Gain | RB-10, W1-05, RB-30 | S〜M | |
| RB-35 | [P1] ボーカル v1 と正解セット | RB-30 | L | |
| RB-36 | [P1] キュー提案と Auto Cue（提案として出し、採用したものだけ書く） | RB-34, RB-15 | M | |
| RB-37 | [P1] 解析設定 UI と Analysis Lock | RB-13, RB-32 | S〜M | |
| RB-38 | [P1] PHRASE EDIT | RB-21, RB-13 | M | |
| RB-39 | [P1] 波形の色モード、全面 / 半面、Preview 列 | RB-20, RB-31 | S | |
| RB-23 | [P1] Rating / Color / Comment / MyTag とマスター連動の Track filter | RB-13, RB-05 | M | |
| RB-45 | [P1/P2] Intelligent Playlist、RELATED TRACKS の条件、Matching | RB-23 | M〜L | |
| RB-41 | [P1] エンジン：拍と小節頭まで合わせる BEAT SYNC、実効キーと連動する KEY SYNC | W3-01, RB-32 | M | |
| RB-43 | [P1] エンジンの波形 Pyramid を PDW3 に | W3-01, RB-20 | M | |
| RB-42 | [P1/P2] エンジン：新しいパッドモード（MEMORY CUE、SLICER、ACT. CENSR、KEYBOARD）、Smart Cue、Gate Cue、Instant Doubles、ループ 1/64〜512 | W3-01, RB-15 | L | |
| RB-46 | [P2] Automix と MIX POINT LINK | W3-01, RB-36 | L | |
| RB-44 | [P2] v2 の小型モデル（各利用者の PC で、その人の検証済みラベルから学習。ONNX → tract） | RB-32〜35 | XL | |

### 9.1 Rust 移行との関係

- 移植は「既定でバグも含めて Python と同じ」が原則。rekordbox 同等化の作業は**追加分**として扱い、パリティの比較から外す（`deviations.yaml` に追加として記録）。
- S5 は既存の挙動（PQTZ、キュー A〜H の取り込み、時刻補正 v1、XML）のまま移植し、土台に RB-10 / RB-11 を使う。その後に RB-13。キュー取り込みの意味が変わる（rekordbox 層を生の時刻で持つ）ことは台帳に載せる。
- S4 / S6 の推薦にはキー相性フィルタを含める（Python に先に入れた場合はコーパスに加えてから移植）。
- S7 の waveform-detail はパリティのために移植し、PDW3 のエンドポイントは追加分（RB-20）。
- S11 で自前パイプライン（RB-40）を Analyzer に接続する。合格基準を満たすまでは暫定の Python ワーカーで S13 に進める。

### 9.2 リスク

| リスク | 対策 |
|---|---|
| rekordbox の形式が変わる | 未知のタグを読み飛ばし、版を確認して警告。合成 ANLZ の CI と、版が上がったときのオーナー環境での統計照合 |
| 時間軸の補正の誤りで、グリッド・キュー・波形がずれる | RB-12 の合格基準（中央値 ≤3ms、PWV7 で ≤1 列）。補正前の値を常に持つので、表の版を上げれば直せる |
| rekordbox の誤りまで真似る | 人の裁定と、キーの合意セット |
| 法務（rekordbox の出力や商用音源での学習） | v1 は DSP と少数の較正のみ。学習は利用者の端末で、その人のラベルから |
| 「rekordbox 相当の UI」が主観に左右される | オーナーの画面と並べて比べるチェックリストを合格条件に |
| 範囲が広がりすぎる | P0 を先に終え、P1 / P2 はその受け入れ後に |
