# JUNCTION 転換フロー再設計 設計書（フェーダースタート方式）

作成日: 2026-09-17。`CURRENT.md`（plumdeck/Mac）・`LITE.md`（share-musics/PlumDeck Lite）の実読結果に基づく。D1〜D4（ユーザー承認済み決定事項）を制約として、既存実装からの移行パスを設計する。

---

## 0. 設計方針の要約

- 交代の判定は「ローカル出力（JUNCTION MASTERを除く）が無音→有音に変わった瞬間」。既存の`Authority`の多段階（preparing→fenced→committed→switching）は**通常フローからは撤去**し、事後整合（tail・解放条件）で安全性を担保する。
- `selectLiteOwner()`（plumdeck, フェンスなし即時切替）と Lite の `switchHostOwner()`（即時`ownerPeerId`書き換え）は、いずれもフェーダースタートに最も近い既存実装であり、**両方をテンプレートとして一般化**する。Mac↔Mac・Mac↔Lite・Lite↔Liteの全組み合わせをこの系統へ統一する（D2）。
- 状態移送（`ReplayDriver`によるグラフ/DSP複製）は通常フローの入力にしない。復旧（recovery）は独立した経路として残す。両者は`Authority`の`phase`/`epoch`/`cutoverFrame`を共有しているため、フィールドの再定義ではなく**新フィールドの追加**で分離する（4章で詳述）。

---

## 1. 状態モデルと既存フィールドの対応

### 1.1 新状態（UI信号灯）

| 状態 | plumdeck (Mac) | PlumDeck Lite |
|---|---|---|
| OFF | セッション未参加、または`rosterOrder`に自分が含まれない | `JunctionRuntime`未生成、または承認待ち |
| STANDBY | `TurnState.phase == "standby"`（新設、2.1節） | `operating==false && receivingJunction==true` |
| READY | `TurnState.phase == "ready"` | `operating==false && readyReasons.length==0` |
| ON AIR | `auth.owner == auth.local` | `operating==true` |
| OUTGOING | `auth.local == releasingPeer`（既存フィールド流用） | `outgoingPeerId==self || hostOutgoing==true`（既存フィールド流用） |

**二重の真実を作らない方針**: 新状態は既存フィールドの単純関数として導出する（新しいenumを別に真実として持たない）。具体的な導出ロジックは`TurnState::derive(auth, runtimeImpl)`という純粋関数として新設し、`program_mixer.h`の`ProgramMixerRoute::resolve()`と同様に「既存状態から見た目の状態を計算するだけ」の設計にする（判断基準4「状態の二重管理を作らない」）。

### 1.2 `Authority`への変更（最小差分方針）

既存フィールドは温存し、新フィールドを追加する形にする（既存の`phase`値集合を壊すとrecoveryの`route()`内`allowed()`ラムダ（`runtime.cpp:1469`）や`authorize()`（`authority.cpp:19-20`）が同時に壊れるため、CURRENT.md 4章の指摘どおり変更は避ける）。

- 追加: `QString turnPhase`（値: `"off"|"standby"|"ready"|"onair"|"outgoing"`）。既存`phase`（`"playing"|"preparing"|"fenced"|"committed"|"switching"|"recovery"`）とは**独立した軸**として扱う。フェーダースタートの通常フローでは`phase`は常に`"playing"`のまま推移し（`preparing/fenced/committed/switching`には遷移しない）、`turnPhase`だけが動く。`recovery`のみ既存の`phase=="recovery"`を継続利用する（障害時は别系統のため）。
- 追加: `QStringList readyBlockers`（READYでない理由。UIの「まだ本番に出ていません：理由」に1件だけ表示するため、優先順位付きリストの先頭を使う）。
- 流用: `owner`, `next`, `releasingPeer`, `rosterOrder`, `finishedOrder`, `turnRequests`, `epoch`（音声ブロックの世代タグとしてそのまま使う。フェーダースタートでも「今どの世代の音がProgramに乗っているか」の曖昧さ防止に必要）。
- 廃止（通常フローの入力としては使わない。フィールド自体は削除せず、recoveryからの到達性のみ残す）: `handoffId`, `fenceFrame`, `cutoverFrame`, `throughSeq`, `committed`（`HandoffCommitMessage`）。理由: これらは「準備→フェンス→検証→コミット→未来フレームでのカットオーバー」という、今回撤去する多段階プロトコルのためのフィールド。T2.3で状態移送を切り離した後、参照が完全にゼロになったことをテストで確認できた範囲だけをT5.1で削除する。

### 1.3 `Runtime::Impl`への変更

- `inputPeer`, `releasingPeer`, `InputRelease inputRelease`: そのまま流用（D3のtail解放条件そのもの）。
- `prepared, ready, aligned, finalCheckpoint, graph, preparedGraph`: 状態移送専用フィールド。通常フローでは**セットしない**（値は常に初期値のまま）。T2.3でMac↔Mac通常フローが新方式に載った後、これらへの書き込みがrecoveryパス以外に残っていないことをテストで確認し、T5.1で削除候補にする。
- 新設: `LocalAudibility localAudibility`（`InputRelease`と対の「ローカル出力＝JUNCTION MASTER以外が無音→有音に変わったか」を追跡する構造体。7章で詳述）。
- 新設: `TailMask tailMask`（tailに含めるデッキチャンネルの集合。D3の送出マスク）。

### 1.4 Lite側 `JunctionRuntime`への変更

既存フィールドは全て流用可能（LITE.md 1.2節の対応表どおり、Liteは元々中間フェーズがないため変更が小さい）。

- 追加: `turnPhase`（Mac側と同じ値集合の文字列。DataChannelの`owner`メッセージに相乗りさせて配信）。
- 追加: `readyBlockers: string[]`。
- 流用: `ownerPeerId`, `outgoingPeerId`, `hostOutgoing`, `senderPeerIds`, `ownerRevision`, `AUTO_RELEASE_MS`。
- 新設: `tailDecks: string[]`（tailに含まれるデッキ、Liteは2デッキなので`"A"|"B"`の部分集合）。

### 1.5 役割とロール

- **ホスト**: 既存`auth.host`, `hosting`フラグをそのまま「会場出力の所有者・順番の管理者」として使う。
- **ON AIR**: `auth.owner`（既存）。
- **次のDJ**: `auth.next`（既存。ただし用途を「フェンス中の後継」から「列の先頭＝STANDBY対象」に変える）。
- **OUTGOING**: `releasingPeer`（既存、Mac）／`outgoingPeerId`or`hostOutgoing`（既存、Lite）。
- **順番待ちの列**: `rosterOrder`（既存、そのまま「表示順＝待機列」として使う。現状も先頭が現演奏者という定義なので、`next`は`rosterOrder[1]`として導出できる場面が増える）。

---

## 2. プロトコルメッセージと機能フラグ

### 2.1 新規メッセージ（`session_protocol.h`に追加、`MessageType`拡張）

| メッセージ | 発生元→宛先 | 内容 | 対応する既存メッセージとの関係 |
|---|---|---|---|
| `TurnAdvance` | ホスト→全員 | 列の先頭が変わったこと（解放後の繰り上げ）。`rosterOrder`更新を配信。 | `HandoffPrepare`の役割を代替するが、フェンスを要求しない点が異なる。 |
| `StandbyBegin` | ホスト→次DJ | STANDBY開始通知。J中継の開始トリガー。 | 新設（既存に相当なし、`selectLiteOwner()`の準備段階が暗黙にやっていたことを明示化） |
| `ReadyState` | 各peer→ホスト | `readyBlockers`の変化を通知（遅延計測、無音状態など）。 | 新設 |
| `OnAirDetected` | ON AIR判定を行った当人→ホスト→全員 | 無音→有音検出、または「今すぐON AIR」ボタン。`Authority.owner`を即時書き換え。 | `selectLiteOwner()`のMac↔Mac/Lite↔Lite一般化版。旧`HandoffCommit`のカットオーバー通知を代替するが、未来フレーム指定はしない（検出フレームそのものが境界）。 |
| `TailRelease` | 次DJ→前DJ、または自動 | tail解放（D3の解放条件のいずれか成立時）。 | `input-released`（Lite既存）/ `InputRelease::observe`（Mac既存）をMac↔Mac双方向に一般化。 |
| `HostSignal` | ホスト→全員 | 「あと1曲」「次どうぞ」「少し待って」「OK」のブースの合図。 | 新設（UI要件5章） |
| `TurnForce` / `TurnSkip` / `TurnReleaseForce` | ホストのみ | 強制交代／スキップ／強制解放。 | 新設。旧`handoff.cancel`のホスト強制版に相当するが対象が広い。 |

### 2.2 廃止（通常フローの発行元コードパスから外す）

`HandoffRequest`, `HandoffPrepare`, `HandoffFence`/`HandoffFenced`, `HandoffCommit`, `HandoffActivate`, `handoff.request/accept/cancel`。**メッセージ型・ハンドラ自体は削除しない**（D2の指示どおり、recoveryが依存していないことを確認できるまで残す）。`Authority::prepare/fence/commit`メソッドは通常フローから呼ばれなくなるが、`recovery.resume`専用の内部経路として残る可能性があるため、T2.3の調査結果を待って判断する。

### 2.3 機能フラグ

`peer.hello`に`capabilities: string[]`を追加し、`"fader-start-v1"`を含めることで新方式を交渉する。含まれない相手には「相手のアプリを更新してください」を表示し、`TurnAdvance`/`StandbyBegin`等の新メッセージを送らない（D4準拠、黙って旧方式にフォールバックしない）。Lite側も`peerConnection.ts`のDataChannel初回メッセージに同フラグを含める形で揃える。

### 2.4 Lite側DataChannel拡張

既存`owner`メッセージ（`{ownerPeerId, senderPeerIds}`）に`turnPhase`, `readyBlockers`, `tailDecks`を追加するだけで足りる（新しいメッセージ型を増やさず、既存の1メッセージに乗せる——Liteは元々フィールド追加に強い緩いスキーマなので、判断基準5「実装量が少ない」を優先）。`decks`メッセージ（LITE.md 6章、現状未消費）は2.4節のJ曲情報として正式に仕様化し、受信側処理を追加する（T2.4/T4.2）。

---

## 3. 継ぎ目（seam）アルゴリズム

### 3.1 ホストが受け手の場合（T1.6、既存の一般化）

既存の`seamFrame`/`TakeoverAnchor`（Lite→Mac専用、`runtime.cpp:174-179`）を、送り手がMacゲストの場合にも使えるよう一般化する。`TakeoverAnchor::resolve()`は「JUNCTIONデッキが直前に報告していた再生フレーム」を使うため、送り手がMac/Liteのどちらでも同じ入力（JUNCTIONデッキの報告フレーム）を渡せば動く。変更は`InputSeam`構造体に`oldOwnerKind: Mac|Lite`を追加する程度で済む見込み（判断基準5）。

### 3.2 ゲストが受け手の場合（T2.1/T2.2、新規）

ホストが次のDJ（ゲスト）へJを中継する構成では、次のDJの受信側で継ぎ目を判定できない（ホストが仲介者のため）。そのため：

1. 次のDJはSTANDBY開始時点から**待機ストリーム**（自分のmasterをホストへ送る）を開始する。
2. 待機ストリームの各ブロックに「そのときJで鳴っていた送り手のメディアフレーム番号」をタグ付けする（既存`PcmBlockInfo.epoch`と同様のインラインメタデータ形式を流用）。
3. ホストは`OnAirDetected`受信後、そのフレーム番号より**前**の「Jだけが鳴っていた地点」を境界として、会場出力を「送り手の直送」から「待機ストリーム」へ切り替える。
4. 境界特定ロジックは`TakeoverAnchor::resolve()`と同じ許容帯域（`kMaxSeamLag48k`/`kMaxSeamLead48k`）の考え方を流用し、`GuestSeam`という新構造体（`InputSeam`の中継版）として実装する。

### 3.3 Mac↔Mac最終形（T2.3）

状態移送を切り離した後は、Mac↔MacもLite↔Liteと同じ「即時オーナー切替＋事後の継ぎ目判定」に統一する。既存の48kHz PCM相関検証（`compareAudio()`、CURRENT.md 2章7項）は**フェンス前の検証**を前提にしており、フェーダースタートでは検証してからON AIRにするのではなく、ON AIRにしてから継ぎ目を事後決定するため、通常フローからは外す（この検証ロジック自体は削除せず、recoveryや将来の診断用に残せるか別途確認する）。

### 3.4 Lite側

フレーム精度の継ぎ目管理は導入しない（LITE.md 7章で指摘のとおりWeb Audioでの実装コストが高い）。既存の時間ベースフェード（`JUNCTION_FADE_SECONDS=0.02s`, `JUNCTION_DETACH_MS=200ms`）をそのまま使う。判断基準1（会場の音が途切れない）を満たす精度としては、20ms/200msのフェードで十分と判断する（Macのフレーム精度継ぎ目は「巻き戻り・重複を厳密に防ぐ」ためのものだが、Liteは既にこの精度なしで本番運用されている実績があるため）。

---

## 4. 遅延予算

- 会場出力の遅延予算（現状0.5秒固定、`effectiveMediaFrame = now()+24000`相当）を、STANDBY中に「送り手→次のDJの遅延＋次のDJ→ホストの遅延＋余裕」の実測値から自動決定する仕組みを新設する。
- 実測方法: 既存の`getMediaStats()`（Lite, 1秒間隔）／`MediaTransport`の統計取得（Mac、CURRENT.md未読部分、T1.4で要確認）から RTT を取得し、`ReadyState`メッセージで往復させて片道遅延を推定する。
- 満たせない場合はREADYにしない（`readyBlockers`に`"latency_budget"`を追加）。本番中に遅延予算を変更しない（既存方針の継続）。

---

## 5. ON AIRの検出（フェーダースタート判定）

### 5.1 検出対象

- ローカルチャンネルフェーダー・クロスフェーダー（JUNCTION MASTER自身を除く、既存`AudioBridge::localReturn`と同じ除外定義）
- マイクON
- サンプラー発音
- JUNCTION MASTERのフェーダー・EQ操作（等倍からの変化）

### 5.2 RT安全な実装

既存の`InputRelease::observe()`（`junction_input.h:159-186`）が「可聴かどうか」を判定するロジックを持っているため、これを反転させた`LocalAudibility::observe()`を新設する。オーディオコールバック内ではatomicなピーク値の更新のみを行い（既存`localReturnPeak`フックをそのまま流用、`cmake/target/CMakeLists.txt`ではなく実際には`native/mixxx-engine-host/`配下のビルド設定を要確認——CURRENT.md冒頭注記のパス相違に留意）、無音→有音の判定・`readyBlockers`の更新・メッセージ送信は非RTスレッド側の`tick()`で行う。

- 閾値・持続時間は`junction_input.h`に定数1か所（例: `kAudibleThresholdDb`, `kAudibleHoldMs`）で定義し、C++ユニットテストで固定する（作業ルール「テストを消したり弱めたりしない」）。
- Lite側は`AnalyserNode`または既存の`junctionEffectiveLevel`計算と同じRMS/ピーク方式をA/B本線デッキ側にも追加する（現状はJUNCTION INチャンネルのLEVELしか監視していない、LITE.md 9章1項）。

### 5.3 READY→ON AIRの判定

「無音から有音への変化だけを合図にする」ため、READYになった時点で既に鳴っている場合は`readyBlockers`に`"already_audible"`を追加し「一度フェーダーを下げてください」を表示する。この判定は`LocalAudibility`が持つ直近状態（有音のままREADYに入った）で表現し、新しいフラグを増やさない。

---

## 6. tail送出マスクとロック表（D3）

### 6.1 tailの確定

ON AIR切替の瞬間、`tailMask.decks = { d | d は post-fader Programに出力されていたデッキチャンネル }`（サンプラー・マイクは含めない）。この集合は**切替後は変化しない**（新たに再生開始したデッキを後から追加しない）。

### 6.2 送出レベルの固定

`tailMask.frozenGain[d] = 切替時点のフェーダー・EQ・マスター音量の実効ゲイン`。以後、前DJがマスター音量・ブース音量つまみを動かしても`frozenGain`は再計算しない（`ProgramMixerRoute`から見ると、tailデッキは「固定ゲインの直接パス」として扱う）。

### 6.3 操作ごとの可否表

| 操作 | tailデッキ | tail以外のデッキ |
|---|---|---|
| ループ設定・自動ループ・ループ解除 | 許可 | 許可 |
| 再生／一時停止 | 禁止 | 許可 |
| シーク | 禁止 | 許可 |
| ホットキュー | 禁止 | 許可 |
| ジョグ／ナッジ | 禁止 | 許可 |
| テンポ・ピッチ | 禁止（SYNC解除・固定、6.4節） | 許可 |
| キー | 禁止 | 許可 |
| SYNC | 禁止（強制解除済み） | 許可 |
| ロード／イジェクト | 禁止 | 許可 |
| フェーダー・EQ・フィルター・FX・クロスフェーダー割り当て | 禁止 | 許可（ただし送出マスクにより会場には出ない） |
| マスター系（マスター音量・ブース音量） | 禁止（tailの送出レベルには無関係。全体マスターの意味では前DJ自身のモニターにのみ影響） | 許可 |

実装箇所: `Authority::authorize()`にtail用の分岐を追加（既存の「引き継ぎ中です」メッセージとは異なる文言、例:「このデッキはOUTGOING中です。ループ以外は操作できません」）。UI側は`program-mixer.ts`の`stripActions()`と同様の禁止理由をツールチップに出す。

### 6.4 SYNCの固定

切替時点で`tailMask`内デッキの`syncEnabled=false`に強制し、テンポを固定する。準備用デッキ（次のDJがこれから使うデッキ、または前DJがtail以外で新たに用意するデッキ）のSYNC操作やリーダー変更が、Mixxxの内部SYNCグループ経由でtailデッキのテンポへ波及しないよう、tailデッキを**別のSYNCグループ（またはSYNCグループから除外）**にする。実装箇所は`mixxx_backend.cpp`のSYNC関連呼び出し（未読、T1.5で要確認）。

### 6.5 解放条件（既存流用＋新設）

| 条件 | 既存流用/新設 |
|---|---|
| Jを下げ切り無音1.5秒 | 既存`InputRelease`（Mac）/`AUTO_RELEASE_MS`（Lite）そのまま |
| tailデッキ全停止または曲終わりで送出が約3秒無音 | 新設（`tailMask`の全デッキが停止/EOTかつpost-fader無音を検出するタイマー） |
| 既存の上限時間 | 既存の`releaseTimer`（Lite 5分）/ Mac側相当（要確認、CURRENT.mdに明記なし＝T1.5で調査） |
| 次のDJの「前のDJを解放」操作 | 既存`releaseInput()`（Lite）/ `JunctionInputDeck.tsx`の解放ボタン（Mac）をそのまま新方式のトリガーに接続 |
| ホストの強制解放 | 新設 `TurnReleaseForce` |

解放後は全ロック解除・送出停止（`tailMask`をクリア）。次に列の先頭に当たる人のSTANDBYは**解放の後**に開始する（同時に2本のJを扱わない、B2Bも同規則）。

---

## 7. 順番の自動化

- ホストが`rosterOrder`を編集（ドラッグ）。
- 「順番に入る」は既定で`rosterOrder`末尾に追加。
- ON AIR中のDJの位置だけ固定（`rosterOrder[0]`が動かないよう、並べ替えAPIで先頭を除外）。
- 演奏済みDJ（`finishedOrder`）も再度`rosterOrder`へ挿入可能にする（既存`finishedOrder`は「もう一度指名されうる」設計のまま活用）。
- B2Bの繰り返し設定: `rosterOrder`に同じpeerを複数回登録できるようにする（A,B,A,B…）か、専用の`repeatPattern`フラグを追加するかはT1.2で実装時に決定する（判断ログに残す）。

---

## 8. 障害時の動き

| ケース | 動き |
|---|---|
| ON AIRのDJが切断 | READYの次のDJがいれば即ON AIRへ切替（ホスト設定で確認/自動）。いなければ既存recovery（`beginRecovery`/`recovery.resume`）を使う。 |
| OUTGOINGのDJが切断 | Jが無音になるだけ。アンダーラン時は短いフェード、自動解放（既存`InputRelease`のストリーム断判定を流用）。 |
| 次のDJが切断 | 列の次を繰り上げ（`rosterOrder`から除去して次を`next`に）、ホストへ通知（`HostSignal`か専用通知）。 |

既存recoveryパスは変更しない（4.1章の分離方針どおり、`turnPhase`とは独立に`phase=="recovery"`のまま動く）。

---

## 9. 廃止・残置の一覧

### 廃止（通常フローからのみ撤去、コードは残す。T5.1で削除判断）

- 状態移送: `startExport()`, `restoreJunctionGraph()`, `tryCaptureGraph()`, `tryFinalizeGraphRestore()`, `ReplayDriver`のフェンス経由呼び出し
- 音声照合: `beginValidation()`/`finishValidation()`/`checkValidation()`/`compareAudio()`
- 未来フレームでのcutover: `Authority::commit()`が設定する`cutoverFrame`の待機ロジック（`advance()`のフレーム比較部分）
- 旧handoff操作: `handoff.request/accept/cancel`（UIボタンとしては撤去、メッセージハンドラは残置）
- ファイル転送: 通常フローでの音源ファイル送信経路（存在すれば。CURRENT.md/LITE.mdともに現状「音源ファイル自体は転送していない」ため、新規に転送機能を作らないことの確認のみで足りる可能性が高い＝T0.1/T0.2の追加調査結果次第）

### 残置（独立して動作を維持）

- 障害復旧: `beginRecovery`, `recovery.resume`, `backupOwner`/`backupEpoch`
- Lite関連の即時切替: `selectLiteOwner()`, `switchHostOwner()`（一般化のテンプレートとして拡張するが削除しない）
- `seamFrame`/`TakeoverAnchor`/`InputSeam`（一般化して存続）

### UI表示の廃止

- 「演奏希望→指名→準備OK・引き継ぐ／交代を確定」ボタン群（`JunctionPanel.tsx`の`onChooseParticipant/onAcceptHandoff/onCancelHandoff`、Liteの`setOwner()`の役割は「強制交代」相当として残るがREADY自動切替が既定になる）
- LOCAL NEXTのデッキ選択UI（`JunctionPerformanceStrip.tsx`の`dj-segmented`）: 廃止し「手元のデッキ全部が対象」に変更
- Junction Liveカード（`JunctionTrackList.tsx`相当）: JのデッキUIへ統合

---

## 10. テスト計画

- **C++ユニットテスト**（`junction-core-tests`に追加）: `TurnState::derive()`の状態遷移網羅、`LocalAudibility::observe()`の無音→有音判定、tailマスクのゲイン固定、SYNC強制解除。
- **TS契約テスト**（`junction:test:contract`に追加）: `program-mixer.ts`の新ステージ導出、`readyBlockers`の優先順位ロジック。
- **既存回帰テスト**: `junction:test:manual`/`junction:test:integration`はrecoveryパスが独立して動くことの確認に使う（状態移送を切り離した後、これらが引き続き通ることをT2.3で確認）。
- **Lite側**: `frontend/tests/junction-audio-routing.test.mjs`と同形式（構造契約テスト）で新しいDataChannelフィールド・tailロジックを追加検証。**現状Lite側は`test`スクリプトが`package.json`に無い（LITE.md 8章）ため、T4.1着手時に併せて`npm test`スクリプトを追加することを検討する（要判断ではないが、判断ログに記録の上で進める）。**
- **未検証として進める項目**: 実音声試験（BlackHole等が必要）、DDJ-1000実機、複数Mac間の実ネットワーク。`docs/junction-transition/MANUAL-CHECK.md`（T5.4）にまとめる。

---

## 11. 移行手順（概要）

1. フェーズ1（T1.x）: ホストが受け手になる場合の核を先に作る（既存の`selectLiteOwner()`一般化が土台になるため、実装量が少ない側から着手）。
2. フェーズ2（T2.x）: ゲストが受け手になる場合、Mac↔Mac統一。状態移送の切り離しは**フェーズ1・2の新フローが全テストを通過し、recoveryが依存していないことを確認できてから**（D2の明記どおり）。
3. フェーズ3（T3.x）: UI。フェーズ1・2の状態モデルが固まってから着手（UIの信号灯はサーバー側状態の単純表示のため、土台が先）。
4. フェーズ4（T4.x）: Lite側。Liteは元々フェーダースタートに近いため、Mac側のプロトコル（`turnPhase`, `readyBlockers`, tail仕様）が固まった時点で並行着手可能。
5. フェーズ5（T5.x）: 削除・ドキュメント更新・全テスト。

---

## 12. D1〜D4との整合性チェック（自己確認）

- **D1（フェダースタートが既定、保険ボタンあり、旧手続き廃止）**: 5章・9章で対応。整合。
- **D2（JUNCTION MASTER方式へ統一、状態移送を通常フローから外す、ファイル転送しない、bootstrapも同方式、recoveryは独立温存）**: 1〜3章・8〜9章で対応。`turnPhase`を`phase`と独立させることでrecoveryとの結合リスク（CURRENT.md 4章の指摘）を吸収。整合。
- **D3（tailの範囲・送出マスク・許可/禁止操作・SYNC固定・解放条件・解放後のSTANDBY順序）**: 6章で対応。許可/禁止表はD3の記述をそのまま反映。整合。
- **D4（接続までの流れは変更しない）**: 本設計は接続後の状態遷移・音声経路のみを扱っており、招待/応答/PlumDeck Lite経由の申請/許可フローには触れていない。整合。

矛盾は検出されなかった。ただし以下は**要判断ではないが実装時に確定が必要な未決事項**として次タスクへ引き継ぐ（判断基準に従い、進行可能なタスクを止めない範囲で先に進める）:

- B2Bの繰り返し表現方法（`rosterOrder`の重複登録 vs `repeatPattern`フラグ）→ T1.2
- Mac側の「既存の上限時間」の解放タイマーの正確な値・実装箇所（LITE.mdの5分と対応するか）→ T1.5
- `decks`メッセージの正式スキーマ（現行フィールドを流用するか再定義するか）→ T2.4
- share-musics `requirements.md`のFR-30とスコープ外記述の矛盾の解消→ T4.4（D1〜D4とは無関係な既存ドキュメント不整合のため要判断には計上しない）
