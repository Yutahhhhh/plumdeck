# Junction 現状調査（フェーダースタート方式への再設計 前提資料）

調査日: 2026-09-17。ブランチ `feat/junction-fader-start`（`main` から作成済み、リポジトリ `/Users/horiyuuta/Workspace/plumdeck`）。本ドキュメントはコードの実読に基づく。読み切れなかった箇所・未確認の推測は明示する。

## 冒頭の注記（指示との相違点）

- `cmake/target/CMakeLists.txt` は存在しない。実際のパスは `native/mixxx-engine-host/cmake/target/CMakeLists.txt`。
- Junctionのテストターゲット定義（`junction-core-tests`）は `cmake/target/CMakeLists.txt` ではなく `native/mixxx-engine-host/CMakeLists.txt`（リポジトリ直下ではなく `native/mixxx-engine-host/` 直下）にある。Junctionのソースリスト自体は `native/mixxx-engine-host/cmake/junction.cmake` で定義され、`plumdeck-junction-core` という静的ライブラリにまとめられている。
- `junction-fx-tests` は `native/mixxx-engine-host/cmake/keylock-checkpoint.cmake` で `EXCLUDE_FROM_ALL` として定義されており、`ctest`（`add_test`）には登録されていない（手動ビルド・手動実行が前提）。
- `native/mixxx-engine-host/src/junction/` 配下のファイル名は指示とほぼ一致するが、`program_mixer.h` のみ存在し対応する `.cpp` はない（ヘッダオンリーの純粋関数クラス）。加えて `program_output.h/.cpp`、`manual_exchange.h/.cpp`（手動招待/応答の交換パケット）、`ids.h/.cpp`（ID・乱数・64bitエンコード規約）など、指示になかったファイルが多数存在する。実際の全ファイル一覧は本文中に記載。

---

## 1. 状態機械

### 1.1 `Authority`（`native/mixxx-engine-host/src/junction/authority.h` / `.cpp`）

ハンドオフの権限状態機械そのもの。フィールド（すべて値型、`Runtime::Impl` に埋め込み）:

- `QString sessionId, host, local, owner, next, handoffId`
  - `owner`: 現在の演奏担当（Program操作権を持つDJ）のpeerId。
  - `next`: 準備中/フェンス中の後継DJのpeerId。
  - `handoffId`: 1回のハンドオフ試行を識別する乱数ID（`secureRandomHex(16)`）。
- `quint64 epoch=1, throughSeq=0, revision=0, fenceFrame=0, cutoverFrame=0`
  - `epoch`: 演奏担当が変わるたびに増分する世代番号。音声ブロック（`PcmBlockInfo.epoch`）と紐付き、新旧ストリームの取り違えを防ぐ。
  - `throughSeq`: フェンス時点までに適用された演奏コマンドの連番（`AppliedStageCommand.sequence` 相当、`Runtime::Impl::controlSeq`）。
  - `fenceFrame`: 旧オーナーが共有操作の受付を止めたメディアフレーム（F0）。
  - `cutoverFrame`: 実際に所有権が切り替わるメディアフレーム（H）。
  - `revision`: UI/wireに送るスナップショットの再取得トリガー用の単調カウンタ。
- `QString phase` （文字列で表現される主状態。値: `"playing" | "preparing" | "fenced" | "committed" | "switching" | "recovery"`）
- `bool localPrep`: このコンピュータがLite DJの受け皿で、自機デッキはローカル試聴専用であることを示す。
- `bool sending`: 操作権を失った直後で、まだ自機マスターを送出中（解放待ち）であることを示す。
- `std::optional<HandoffCommitMessage> committed`: 確定済みコミットの内容（`session_protocol.h` 参照）。

状態遷移メソッドと呼び出し契機:

- `prepare(target)`: `phase=="playing"` のときだけ許可。`phase="preparing"`, `next=target`, `handoffId` 新規発行。呼び出し元は `Runtime::command("session.start"/"handoff.request")`（ホスト側、`runtime.cpp:1955-1967`, `1991`）。
- `fence(frame, watermark)`: `phase=="preparing"` のときだけ。`phase="fenced"`, `fenceFrame`, `throughSeq` を設定。呼び出し元は `Runtime::command("handoff.accept")`（`runtime.cpp:1993-1997`）、および `finishBootstrap()`（最初のDJ起動時のブートストラップ用フェンス、`runtime.cpp:1540-1546`）。
- `commit(HandoffCommitMessage, sender)`: `sender==host` かつ `phase=="fenced"` などの一致条件で `phase="committed"`, `cutoverFrame=m.effectiveMediaFrame` を設定。ホスト自身は `applyCommit()`（`runtime.cpp:1512-1520`, 呼び出し元 `checkValidation()` と `finishBootstrap()`）経由、ゲストは `MessageType::HandoffCommit` 受信時（`runtime.cpp:1131`）に適用。
- `cancel()`: `phase` が `preparing`/`fenced` のときのみ、確定前なら取り消し可能。`Runtime::command("handoff.cancel")`（`runtime.cpp:1992`）、タイムアウト（`fenceDeadline`/`startDeadline`超過、`runtime.cpp:1575`, `1629`）で自動呼び出し。
- `advance(frame)`: `phase=="committed"` かつ `frame>=cutoverFrame` で `owner=committed->newOwner`, `epoch=committed->newEpoch`, `phase="switching"` に進める。毎tick `runtime.cpp:1589` で呼ばれる（`d->auth.advance(now())`）。
- `recover(producer, newEpoch, frame)`: ホストのみ。`phase="recovery"` にして `owner=producer, epoch=newEpoch, cutoverFrame=frame`。`Runtime::command("recovery.resume")`（`runtime.cpp:1981-1989`）から呼ばれる。

`authorize(op, ticket, frame)`（`authority.cpp:13-22`）が操作権チェック本体:

1. `localOnly(op)` または `readOnlyQuery(op)`（後述）は無条件許可。
2. `ticket.sessionId`/`actorPeerId` が現在のセッション/自分と一致しなければ「共有セッションの操作情報が更新されています」。
3. `ticket.epoch` が現在の `epoch` と一致しなければ「交代前の操作は適用できません」。
4. `local!=owner` かつ (`!localPrep || sending`) なら「現在のプレイ担当者だけが操作できます」。
5. `phase=="recovery"` なら「配信を復旧中です」。
6. **`(phase=="fenced" || phase=="committed") && frame>=fenceFrame && (!committed || frame<cutoverFrame)` の場合に「引き継ぎ中です。操作をもう一度行ってください」**（詳細は2章）。

`localOnly` / `readOnlyQuery` は固定の操作名セット（`authority.cpp:3-12`）。前者はマイク以外のローカル専用操作（録音、サンプラーPFL等）、後者は波形リース等の非破壊クエリで、権限ゲートを迂回する。

### 1.2 `Runtime::Impl` の追加状態（`native/mixxx-engine-host/src/junction/runtime.cpp:102-220`）

`Authority` 以外に、`Runtime::Impl` 構造体（`runtime.cpp:102`）が保持する主要フィールド:

- `QString lifecycle` = `"lobby" | "starting" | "live"`（`runtime.cpp:142`）。セッション全体のライフサイクル。
- `QStringList rosterOrder, finishedOrder`（`runtime.cpp:142`）: `rosterOrder` は表示順（先頭が現演奏者）、`finishedOrder` は「一度演奏し終えたDJ」のFIFO的キュー（もう一度指名されうる）。
- `QSet<QString> turnRequests`（`runtime.cpp:142`）: 「演奏を希望」した peerId の集合。ホストの `handoff-request`/`HandoffRequest` メッセージ受信時に挿入（`runtime.cpp:703`, `1074`）。指名（`prepare`成功）時に該当peerを除去。
- `QString inputPeer, releasingPeer`（`runtime.cpp:162`）: `inputPeer` はJUNCTIONデッキ（後述）に音を出しているpeer、`releasingPeer` は「操作権は移ったがまだフェードアウトされていない旧担当者」。
- `InputRelease inputRelease`（`junction_input.h:159-186`）: `releasingPeer` の自動解放判定（フェード検出・タイムアウト）を持つ状態機械。
- `bool inputMainMix`: JUNCTION MASTER（Auxiliary1）がメインバスに入っているかのミラー。
- `std::optional<InputSeam> seam` + `std::atomic<quint64> seamFrame` + `std::atomic<bool> takeoverAnchor`（後述5章）。
- `bool prepared, ready, aligned, finalCheckpoint`: 演奏状態の引き継ぎ（グラフ転送）パイプラインの各段階完了フラグ。
- `QJsonObject graph, preparedGraph`: エクスポート/インポートされる演奏グラフ（4デッキ・サンプラー・DSP資産参照）。
- `quint64 recoveryUntil, recoveryResumeFrame, recoveryEpoch; QString backupOwner; quint64 backupEpoch`（`runtime.cpp:157-158`）: 音声レベルの障害復旧用（4章）。
- `bool hosting, manual, liteSession`: それぞれ「自分がホスト（コーディネータ）か」「シグナリングサーバー不使用の手動交換モードか」「PlumDeck Liteとして参加しているか」。

状態遷移を起こす主なメッセージ/操作名（native側 `MessageType` は `session_protocol.h:24-53`、JS側コマンド名は `Runtime::command()` の `op` 文字列）:

| メッセージ/操作 | 発生元 | 効果 |
|---|---|---|
| `session.start` | ホストUI | `auth.prepare(target)`、`lifecycle="starting"`、`bootstrapStart=true`（最初のDJ起動） |
| `handoff.request` | 現/次DJ、または一般DJ（「演奏を希望」） | ホストなら `auth.prepare(target)` して `handoff.prepare` を配信、非ホストならホストへ転送。`targetPeerId` 省略時は自分を指名（＝希望） |
| `HandoffPrepare` | ホスト→全員 | 受信者側 `auth.next/handoffId/phase="preparing"` を反映、グラフのエクスポート/転送準備開始 |
| `handoff.accept` | 次DJ→ホスト（`handoff.ready{requestFence:true}`）／ホスト自身 | ホスト側で `ready==true` を条件に `auth.fence()` を呼び `handoff.fence` を配信 |
| `HandoffFence`/`HandoffFenced` | ホスト→次DJ／次DJ→ホスト | `auth.fence()` 適用、`fenceDeadline` セット |
| （内部）検証成功 | `checkValidation()` | `applyCommit()` → `HandoffCommit` を配信 |
| `handoff.cancel` | ホストUI／タイムアウト | `auth.cancel()`、`preparing`/`fenced`中のみ可 |
| `recovery.resume` | ホストUI（復旧中のみ） | `auth.recover()` 相当（実装は `runtime.cpp:1587` のtick内で直接フィールド操作） |
| `lite.owner.set` / `selectLiteOwner()` | Lite関連（フェーダースタート的な即時切替） | Liteが絡む場合は上記フェンス方式を使わず、`selectLiteOwner()`（`runtime.cpp:662-698`）が **即座に** `auth.owner` を切り替える。既存コードで唯一「フェンス・検証なし」の即時オーナー切替経路（5章参照）。既に一部フェーダースタート的挙動が実装済みであることに注意。|

---

## 2. 交代（ハンドオフ）フロー

Mac↔Mac（フル機能ホスト同士）の場合、「演奏希望→指名→準備OK→交代確定」は次のシーケンス（すべて `runtime.cpp` 内、ファイル/関数名を付記）:

1. **演奏希望**: 非オーナーDJが `handoff.request`（`targetPeerId` 省略）を送る → ホストで `HandoffRequest` 受信（`runtime.cpp:1074`）→ `turnRequests.insert(id)`。UIには `rosterStatus="requested"`（`runtime.cpp:311`）として表示される。
2. **指名**: ホストUIが特定DJを選び `handoff.request{targetPeerId}` または `session.start{performerPeerId}`（最初の一人のみ）を呼ぶ → `Authority::prepare(target)` 成功 → `phase="preparing"`、`handoffId` 発行 → `broadcast("handoff.prepare", {targetPeerId, handoffId})`（`runtime.cpp:1991`）。
3. **グラフのエクスポート**: 現オーナー側で `startExport()`（`runtime.cpp:1194-1222`）が非同期（`std::async`）に4デッキ・ミキサー・サンプラー・DSP資産を含む演奏グラフを構築し、`sendGraph()`（`runtime.cpp:1156-1162`）でハッシュ化ファイルとして `graph.manifest`（`MessageType::GraphManifest`）を送る。受信側は `requestAssets()`→`tryPrepare()`（`runtime.cpp:1223-1234`）で `backend->restoreJunctionGraph()` を呼び実際にネイティブ側へ復元する（詳細は4章）。
4. **準備OK**: 次DJ側で `prepared && aligned && backend->junctionGraphReady()` が揃うと `ready=true` にして `handoff.ready{ready:true, stage:auth.phase, throughSeq}` をホストへ送る（`runtime.cpp:1615-1621`）。UI側の「準備OK・引き継ぐ」ボタン（`ExchangeFlow`ではなく `program-mixer.ts:stripActions()` の `accept` アクション、`disabled: !snapshot.readiness.ready`）はこの `ready` フラグを見ている。
5. **フェンス**: ホストがUIで「交代を確定」（同じ `handoff.accept` 操作、hosting分岐）を押す、または非ホストの次DJが `handoff.accept` を押すと `queue("handoff.ready", {requestFence:true})` でホストに委任 → ホストが `auth.phase=="preparing" && ready` を確認して `auth.fence(now()+4800, controlSeq)` を実行、`fenceDeadline` を6秒後に設定、`handoff.fence` を配信（`runtime.cpp:1993-1997`）。
6. **フェンス中の操作ブロック**: フェンスされた瞬間から実コミットまでの間、現オーナー（まだ `owner` のまま）が演奏操作を送ると `Authority::authorize()` が **「引き継ぎ中です。操作をもう一度行ってください」** を返す（`authority.cpp:20`）。条件は `(phase=="fenced"||phase=="committed") && frame>=fenceFrame && (!committed || frame<cutoverFrame)`。つまり「フェンスされたメディアフレームに達し、かつまだコミットされていない、またはコミット済みでもカットオーバー前」の間はブロックされる。UIはこのエラーメッセージをそのままトーストする想定（`src/services/performance-command-router.ts` はローカルガードのみで、実際の拒否はnative側のこのメッセージに依存）。
7. **音声検証**: フェンス後、`beginValidation()`/`finishValidation()`/`checkValidation()`（`runtime.cpp:1481-1571`）が両者から48kHz 12000フレームのPCM窓を集めて `compareAudio()`（`validation.h`、未読=波形相関/レベル差を見る比較関数と推測）で一致を確認する。一致しなければ最大3回まで再調整（`validationRound`）。
8. **コミット**: 検証成功で `applyCommit()` がディスクへコミット内容を永続化（`QSaveFile` で `<cache>/junction/<sessionId>.commit`）した上で `auth.commit()` を適用、`handoff.commit` を配信。`cutoverFrame` は `effectiveMediaFrame = now()+24000`（24000フレーム=0.5秒後、48kHz換算）。
9. **カットオーバー**: 各peerは毎tick `auth.advance(now())`（`authority.cpp:42-44`）でメディアフレームが `cutoverFrame` を超えた瞬間に `owner`/`epoch` を実際に切り替え、`phase="switching"`。
10. **後片付け**: `phase=="switching"` かつ `now() > cutoverFrame + delay + 48000` で `phase="playing"`（または `lifecycle="live"`）に戻り、`rosterOrder`/`finishedOrder` を更新（`runtime.cpp:1635`）。

なお **PlumDeck Lite が絡む交代**（現オーナーか次オーナーのどちらかが `litePeer()`）は上記のフェンス/検証/コミット手順を一切経由せず、`selectLiteOwner()` が即座に `auth.owner` を切り替える（`runtime.cpp:662-698`, `1991`内の分岐）。これは今回のフェーダースタート方式に最も近い既存の実装であり、「音を出した瞬間に切り替える」という要件と設計思想が一致する数少ない既存パスである。

---

## 3. 音声経路

トポロジーは `program-mixer.ts` の `ProgramVenueSource`/`ReturnSource` とネイティブ `program_mixer.h` の `VenueSource`/`ReturnSource` に対応。

- **JUNCTION MASTER**: 前DJの現在の音を受ける仮想入力チャンネル。ネイティブ実装は Mixxx の `[Auxiliary1]` チャンネルグループ（`mixxx_backend.cpp:76` `kJunctionGroup = "[Auxiliary1]"`、`mixxx_backend.cpp:239-253` で `EngineAux` として登録）。音声データそのものは `junction::JunctionInput`（`junction_input.h/.cpp`）が48kHzのP2P受信ブロックを44.1kHzにサンプルレート変換しながらリングバッファへ蓄積し、オーディオコールバックの `feedJunctionAux()`（`mixxx_backend.cpp:1087-1093`）が `Runtime::readJunctionInput()`→`JunctionInput::read()` で毎ブロック読み出す。
- **LOCAL NEXT**: このDJがこれから演奏するローカルデッキ群。エンジン側は `AudioBridge::localReturn`（`audio_bridge.h:13-21`）という「post-fader local channel、JUNCTION MASTER自身とマイクを除外したバス」で表現される。`localReturnExcluded` にJUNCTION MASTERのチャンネルハンドルが設定される（`mixxx_backend.cpp:247`）。
- **Program Master**: 会場に出る唯一のバス。`ProgramMixerRoute::resolve()`（`program_mixer.h:61-79`）が `VenueSource::LocalMix|DirectStream|RemoteHost|None` を決定する純粋関数。実際の捕捉は `Runtime::capture()`（`runtime.cpp:1706-1742`）がオーディオコールバックのメインバスから受け取り `ProducerTap`→P2P送出、ホストでは `ProgramOutput`（`program_output.h/.cpp`）が会場デバイスへ出力。

トポロジー別の違い（`program_mixer.h` コメントおよび `runtime.cpp` の分岐から）:

- **Mac↔Mac**: フェンス/検証/コミット方式のフルハンドオフ（2章）。音声はWebRTC(Opus)経由、`MediaTransport`（`media_transport.h/.cpp`）が2本のPeerConnection（`control`用と`bulk`転送用、`pc[0]`/`pc[1]`）＋別途 `validation` DataChannelを持つ。
- **Mac↔Lite**: Liteは1本のブラウザ互換WebRTC接続のみ（`MediaTransport::startLite()`、`media_transport.cpp:156-162`）。トラック交代は `selectLiteOwner()` による即時切替（3章末尾参照）。前オーナーの音は `releasingPeer` として残り、新オーナーがフェーダーで下げ切ると `InputRelease::observe()` が解放を検出して `releaseInput()` を呼ぶ（`runtime.cpp:604-612`）。
- **Lite↔Lite**（ホスト経由で2台のLiteが交代）: ホストが `returnRelaySource`/`returnRelayTarget`（`runtime.cpp:173`）を設定し、`route()`（`runtime.cpp:1447-1476`）内で一方のLiteの `decodedRing` から他方のLite用 `liteReturnRings` へ直接転送（`ReturnSource::RelayedPeer`、`program-mixer.ts` の `returnLabel` にも表示される）。
- **戻り経路（returnFeed）**: `relayed-peer` はホストが送出側Liteの音を受け側Liteへ中継するケース。`feedbackBlocked` は「JUNCTION MASTERがメインバスに入っている状態でローカル返送を要求した」場合に立つガード（`program_mixer.h:73-77`）で、前DJに自分の音を送り返すフィードバックを防ぐ。

---

## 4. 状態移送と復旧の依存関係

### 4.1 演奏グラフの転送（ハンドオフ専用、`Runtime`側）

- **エクスポート**: `MixxxBackend::junctionGraph()`（`mixxx_backend.cpp:91-96`）は非同期スナップショット要求をトリガーするだけで、実データは `tryCaptureGraph()`（`mixxx_backend.cpp:1034-1067`）がバックグラウンドで `renderDriver_.requestTransfer(GraphDriver::None, RenderMode::Cold)` によりリアルタイムグラフの所有権を一時的に手放させてから、`snapshotStoppedGraph()`（4デッキのpath/position/beatgrid/controls、`mixer()`、`samplers_->junctionState()`）＋ DSP資産（`junction::ddj::Snapshot`、`junction::keylock::Snapshot`、`junction::fx::Snapshot`）を捕捉する。DSP資産はディスクへ書き出し（`profile_.filePath("junction-dsp.bin")` 等）、ハッシュ付きアセットとして `Runtime::startExport()`（`runtime.cpp:1194-1222`）経由でP2P転送される。
- **インポート**: `MixxxBackend::restoreJunctionGraph()`（`mixxx_backend.cpp:118-156`）がスキーマ検証後 `pendingDsp_/pendingKeylock_/pendingFx_` にセットし、4デッキを `load()`。`tryFinalizeGraphRestore()`（`mixxx_backend.cpp:1100-1147`）が全デッキ準備完了を待ち、`renderDriver_`（`junction::ReplayDriver`、`replay_driver.h/.cpp`）でオフラインWarm-Replay駆動に切り替えて `applyGraphDeck()`/`applyGraphMixer()`/DSP復元を適用し、最後にリアルタイム駆動へ戻す。
- **`ReplayDriver`（`replay_driver.h/.cpp`）**: Mixxxのプロセスグローバルな唯一の `EngineMixer` を「どのドライバが処理するか」で排他制御する状態機械（`GraphDriver::None|Realtime|Replay`、`RenderMode::Cold|WarmOffline|ArmedRealtime|Performing`）。`requestTransfer()`→`transferComplete()` のポーリング型ハンドシェイクで、オーディオコールバック側は `beginBlock()`/`acknowledgeRelease()` のみ（ロックなし）。
- **フェンス下の位置合わせ**: `alignJunctionGraph()`（`mixxx_backend.cpp:157-165`）と `tryFinalizeGraphRestore()` 内の `aligning_` 分岐が、確定した `atMediaFrame` に一致するまでWarm-Replayを進めてから実演奏に切り替える「未来フレームでのcutover」の実装本体。

### 4.2 音声レベルの障害復旧（Recovery、`Runtime::Impl` 内）

- `beginRecovery(reason)`（`runtime.cpp:1433-1442`）は `auth.phase="recovery"` にし、`backupOwner/backupEpoch`（直前のコミット済みオーナー）を記録し、`session.recovery{stage:"active"}` を配信するのみ。**演奏グラフのエクスポート/インポート（4.1節）を一切呼ばない。**
- `recovery.resume`（`runtime.cpp:1981-1989`、ホストのみ）は `recoveryResumeFrame = now()+24000`、`recoveryEpoch` を設定し、ディスクへ復旧レコード（`<cache>/junction/<sessionId>.recovery`）を保存した上で `setCaptureAnchor()` により **ホスト自身の手元の演奏（ローカルデッキ）** をキャプチャ対象に切り替える。つまり復旧は「グラフを転送し直す」のではなく「ホストのローカル演奏をそのままProgramへ差し込む」音声ルーティングレベルの切替。
- `route()`（`runtime.cpp:1447-1476`）内の `allowed()` ラムダが `phase=="recovery"` の間、`recoveryResumeFrame` 前後で `backupOwner/backupEpoch` または `auth.host` のブロックだけをProgramへ通す境界を決める。

**依存関係のまとめ（今後の設計変更で要注意な点）**: 現状、「DJ間のハンドオフ」（演奏グラフ・DSP状態の完全な複製、4デッキ/サンプラー/FX同期）と「障害復旧」（音声ソースの差し替えのみ）は**互いに独立した経路**である。ただし両者は同じ `Authority`（`phase`, `epoch`, `cutoverFrame`, `committed`）と同じ `route()`/`capture()` のフレーム境界ロジックを共有しているため、フェーダースタート方式へ再設計する際に `phase` の値集合（`"preparing"/"fenced"/"committed"` 等）や `Authority::authorize()` の分岐を変更すると、**意図せずrecoveryパスの条件式（`runtime.cpp:1469` の `allowed()` ラムダや `authority.cpp:19-20`）も同時に壊れる**リスクが高い。フェーダースタート化にあたり、グラフ転送（4.1節）を「操作権の移譲」から切り離す場合、recoveryが依存する `auth.committed`/`cutoverFrame` の意味を変えないよう注意が必要。

---

## 5. 継ぎ目（seam）関連

- **`seamFrame` / `InputSeam`**（`runtime.cpp:174-179`, `1592`, `1653`, `1730`）: Lite→Mac引き継ぎ専用の一回限りの継ぎ目。`selectLiteOwner()` がLiteからの即時オーナー移譲を検出すると `seam=InputSeam{oldOwner, oldEpoch}` をセットし、`takeoverAnchor.store(true)` する。実際の境界フレームは `Runtime::capture()`（`runtime.cpp:1706-1742`）内で、新オーナーの最初のキャプチャブロック時に `TakeoverAnchor::resolve()`（`junction_input.h:140-150`）が「JUNCTIONデッキが直前のコールバックで報告していた再生フレーム」を優先して確定し、`seamFrame.store(media)` する。`tick()` 内 (`runtime.cpp:1592`) で `programEnqueuedThrough >= boundary` になったら `seam.reset()`。
- **`TakeoverAnchor`**（`junction_input.h:140-150`）: 「デッキが報告した再生フレーム」が妥当な帯域（`kMaxSeamLag48k`/`kMaxSeamLead48k`）に収まればそれを、収まらなければローカルクロック由来の `elapsed` フレームにフォールバックする純粋関数。
- **`InputSeam`構造体自体**は単に `oldOwner`/`oldEpoch` を保持するだけで、実際の境界判定は `route()` 内の `allowed()` ラムダ（`runtime.cpp:1469`）が `seamFrame` 未満は旧オーナー、以降は新オーナーの音だけをProgramへ通す形で行う。
- **`fenceFrame`/`cutoverFrame`**（Mac↔Mac完全ハンドオフの継ぎ目）は `Authority` のフィールドとして1章・2章で説明済み。`seamFrame`/`TakeoverAnchor` とは別系統で、Lite即時交代専用。
- **`InputRelease`**（`junction_input.h:159-186`）: 継ぎ目そのものではなく「旧オーナーの送出をいつ止めるか」を判定するタイマー（可聴だった後1.5秒無音、ストリーム断2秒、最大5分の3フォールバック）。

---

## 6. UI

- **JunctionBar**（`src/components/junction/JunctionBar.tsx`）: ヘッダーの丸ボタン（`JunctionRootButton`）＋常時マウントされる非モーダルパネル（`JunctionPanel`）。**1秒間隔のポーリング**（`setInterval(() => void poll(), 1000)`, `JunctionBar.tsx:49`）で `junctionCommand('snapshot')` を呼び続けており、プッシュ通知やイベント駆動ではない。加えて `src/services/junction/client.ts:30-46` の `junctionCommand()` は `snapshot` 以外のほぼ全操作の直後にも自動で `snapshot` を再取得する（`op==='input.set'` のみ部分反映で最適化）。フェーダースタート方式では「フェーダーが動いた瞬間の切替」を扱うため、この1秒ポーリング（+コマンド起因の逐次snapshot）の遅延特性がボトルネックになりうる。
- **JunctionPanel**（`src/components/junction/JunctionPanel.tsx`, 628行）: DJ一覧（`JunctionRoster`）、招待/交換フロー（`ExchangeFlow`, `InviteCard`）、DJプロフィール編集（`DjProfileEditor`）、ネットワーク設定（`NetworkSettingsSection`）、Google連携（`ShareMusicsJunction`）をタブ的に内包。ハンドオフ関連ハンドラ（`JunctionPanel.tsx:359-362`）: `onChooseParticipant`→`session.start`/`handoff.request`、`onAcceptHandoff`→`handoff.accept`、`onCancelHandoff`→`handoff.cancel`、`onRequestTurn`→`handoff.request{targetPeerId:self}`。
- **JunctionRoster**（`src/components/junction/JunctionRoster.tsx`）: `roster-model.ts` の `RosterVisualState`（`invited/response/connecting/ready/requested/next/playing/finished/reconnecting/disconnected/problem`）に応じたバッジ表示。「操作権」バッジ（playing）、「送出中」バッジ（`releasingPeer`に相当、sounding）、通信品質インジケータ（`ConnectionIndicator`、`qualityPresentation()` の3段階バー）。
- **JunctionPerformanceStrip**（`src/components/play/JunctionPerformanceStrip.tsx`）: プレイ画面に常設されるJUNCTION MASTER/LOCAL NEXT/PROGRAM MASTER の3セル型ストリップ。**LOCAL NEXTのデッキ選択UI**は `dj-segmented` ロールグループの `A/B/C/D` ボタン（`JunctionPerformanceStrip.tsx:120`, `chooseNextDeck(deck)`）。ステージラベル（`STAGE_LABEL`、`src/services/junction/program-mixer.ts:58-65`）は `inactive/waiting/cueing/mixing/playing/sending` の6状態を日本語文言で表示する、いわば信号灯的なステータス表示。
- **JunctionInputDeck**（`src/components/play/JunctionInputDeck.tsx`）: 波形状のレーン表示（`dj-junction-input-lane`、直近300バケットの音量＋鳴っていたデッキ色）付きのJUNCTION MASTERチャンネルストリップ。解放ボタン（「前のDJを解放」）は `releasing` 時のみ表示。
- **Junction Liveカード**: 明確な単一ファイルはなく、`JunctionTrackList.tsx`（`src/components/play/JunctionTrackList.tsx`、未読=ファイル名から中身は現在/次曲リストと推測）と `src/services/junction/tracks.ts` の `junctionBrowserTracks()`（ホスト側のみ、`showJunctionTracks()` 条件）が該当。
- 上記はすべて `useJunction()`（未読、`src/hooks/useJunction.ts` と推測）フックが `junctionState`（`src/services/junction/state.ts`、単純なpub/subストア）を購読して描画している。

---

## 7. MCP

### 7.1 Python側ツール（`backend/mcp_server/tools/junction.py`）

`@mcp.tool()` デコレータ付き関数は**41個**（`junction_get_state` から `junction_release_input` まで）。ハンドオフ関連は `junction_reorder_roster`, `junction_start_session`, `junction_request_handoff`, `junction_cancel_handoff`, `junction_accept_handoff`, `junction_resume_recovery` の6つ。すべて `_call(action, args)`→`post_junction_action()`（`backend/mcp_server/junction_bridge.py`）経由でTauriのローカルブリッジへPOSTする。

### 7.2 Rust側ブリッジ（`src-tauri/src/junction_mcp_bridge.rs`, 832行）

`127.0.0.1` のみにbindするループバックHTTPサーバー（起動ごとのランダムbearerトークン認証）。`dispatch_action()`（`junction_mcp_bridge.rs:397-580`）の固定allowlistで `action` 文字列→ネイティブ `op` 文字列に変換して `EngineSupervisor::send_current()` へ渡す。allowlistに含まれる主なaction: `prepare, exchange.export, live.attach, live.detach, mic.enabled, snapshot, exchange.inspect, audio.devices, network.*, create, join, invite.*, exchange.import, peer.approve/reject/retry, profile.update, roster.reorder, session.start, handoff.request/cancel/accept, recovery.resume, leave, end, program.*, private.*`。

**重要な発見（未確認・要検証、コード読解に基づく）**: Python側の `junction_configure_input`（action=`"input.set"`）と `junction_release_input`（action=`"input.release"`）は、`junction_mcp_bridge.rs` の `dispatch_action()` allowlist（`junction_mcp_bridge.rs:415-579`）に**該当するアーム自体が存在しない**（ファイル全体を `grep -i input` しても0件）。したがってこの2ツールを呼ぶと `_ => bridge_error("unsupported_action", "対応していないJunction操作です", false)` に落ちる可能性が高い。実際に実行して確認したわけではないため断定はしないが、JUNCTIONデッキ（LEVEL/EQ/L-THRU-R/CUE/解放）のMCP操作とREADMEの記述（後述7.3）は、少なくともこのファイルの現在の内容とは食い違って見える。フェーダースタート方式でJUNCTIONデッキ制御のMCP経路を触る場合はまずこの整合性を確認すべき。

### 7.3 READMEのMCPツール数記述

`README.md:287` に「現在は85ツール（うちJunction 39ツール）を登録し」とある。実際に `backend/mcp_server/tools/junction.py` を数えると **41個**であり、README記載の**39個と2つ食い違う**（README側が更新漏れの可能性）。

---

## 8. MIDI（DDJ-1000）

`src/services/midi/ddj1000-runtime.ts` と `src/hooks/useDdj1000.ts` を確認した限り、**Junctionのハンドオフ操作やJUNCTIONデッキに対する専用の物理コントロール割り当ては存在しない**。存在するのは以下のみ:

- `junctionLeaseKey()`（`src/services/junction/state.ts:15`）を購読し、リースキーが変わる（＝ハンドオフでセッション/epoch/自分の役割が変わる）たびに `epoch++` して `pickup.clear()` する（`ddj1000-runtime.ts:69-72`）。これはMIDIフェーダー/ノブの「拾い上げ（pickup）」状態をハンドオフの前後でリセットするための安全策であり、Junction固有の割り当てではない。
- `routePerformanceCommand()`（`src/services/performance-command-router.ts`）を経由して、Junctionセッション中は `local`（常時許可）セットと `shared`（`localPeerId===performerPeerId || localPrep` のときのみ許可）セットで操作を振り分ける、という一般的な演奏コマンドゲートのみ。DDJ-1000固有のJunctionボタン（例: フェーダー交代トリガーの専用ボタン割り当て）は見当たらない。

---

## 9. テストの実行方法

### 9.1 フロントエンド/JS（`package.json` scripts、`grep junction package.json` で確認した実際の文字列）

- `junction:test:contract`: `pnpm --dir services/junction-signaling build && pnpm --dir services/junction-signaling test && node --experimental-strip-types --test src/services/junction/control-boundary.test.mjs src/services/junction/exchange-actions.test.mjs src/services/junction/roster-model.test.mjs src/services/junction/program-mixer.test.mjs src/services/junction/tracks.test.mjs src/services/junction/share-musics/automation.test.mjs src/services/play-drag-routing.test.mjs`
- `junction:test:ui`: `tsc --noEmit && node --experimental-strip-types --test`（同上のJSテスト群、`.mjs`のみ、契約テストのsignalingビルドを含まない）
- `junction:test:manual`: `node --test native/mixxx-engine-host/tests/junction/manual-runtime.test.mjs`（手動交換方式のNode駆動E2E、ネイティブホストを子プロセス起動すると推測）
- `junction:test:integration`: `pnpm --dir services/junction-signaling build && node --test native/mixxx-engine-host/tests/junction/runtime.test.mjs`（既存サーバー方式の回帰試験、README注記どおり）
- `junction:test:audio`: `node scripts/project.mjs junction-test program-output`
- `junction:test:network`: `pnpm --dir services/junction-signaling build && node scripts/project.mjs junction-test media-transport && pnpm --dir services/junction-signaling test`

### 9.2 ネイティブC++（`native/mixxx-engine-host/CMakeLists.txt:20-24`）

- `junction-core-tests`: `add_executable` で `tests/junction/harness.cpp` ＋ `clock_test.cpp, media_transport_test.cpp, asset_cache_test.cpp, program_output_test.cpp, producer_tap_test.cpp, validation_capture_test.cpp, authority_test.cpp, network_settings_test.cpp, manual_exchange_test.cpp, shared_tracks_test.cpp, junction_input_test.cpp, program_mixer_test.cpp` を1つのバイナリにまとめたもの。`plumdeck-junction-core` にリンク。`ctest` 経由 `add_test(NAME junction-core COMMAND junction-core-tests)`。実行コマンド例（README記載どおり）: `native/mixxx-engine-host/build-junction/junction-core-tests`。
- `junction-fx-tests`: `native/mixxx-engine-host/cmake/keylock-checkpoint.cmake:50-56` で `EXCLUDE_FROM_ALL` 定義、`fx_engine.cpp` を `PLUMDECK_JUNCTION_FX_TEST_MAIN=1` でスタンドアロン実行ファイルとしてビルド。`ctest` には登録されていない（`add_test` なし）。README記載の実行コマンド: `native/mixxx-engine-host/build-upstream/junction-fx-tests`（ビルドディレクトリ名が `junction-core-tests` とは異なる点に注意＝別のCMakeビルドツリー）。
- `protocol-seam`: `native/mixxx-engine-host/CMakeLists.txt:16` で `node --test tests/protocol.test.mjs` を `PLUMDECK_TEST_HOST` 環境変数付きで実行するctestターゲット（Junction専用ではなくシード層全体のプロトコルテスト）。

### 9.3 `backend/tests`

grepで `junction` を含むテストは確認できなかった（未確認＝存在しない可能性が高いが、`backend/tests` 全体の網羅的な検索はしていない）。

### 9.4 CI（`.github/workflows/pr-checks.yml`）

`frontend` ジョブの「MIDI and audio recovery regression tests」ステップで次を実行:
```
node --experimental-strip-types --test src/services/midi/generic-midi.test.mjs src/services/dj-engine/audio-ready.test.mjs src/lib/path-breadcrumbs.test.mjs src/components/docs/topics.test.mjs src/services/junction/control-boundary.test.mjs src/services/junction/exchange-actions.test.mjs src/services/junction/roster-model.test.mjs src/services/junction/tracks.test.mjs
```
（`program-mixer.test.mjs` と `share-musics/automation.test.mjs` はCIのこのステップには**含まれていない**＝ローカルの `junction:test:ui`/`junction:test:contract` にはあるがCIでは走っていない可能性がある。）
続けて「Junction signaling contract tests」ステップで `services/junction-signaling` ディレクトリの `npm ci && npm test`。

**重要な発見**: `pr-checks.yml` 全体をgrepしても `ctest`, `junction-core`, `cmake --build`, `mixxx-engine-host` の文字列は一切出現しない。つまり**ネイティブC++側のJunctionテスト（`junction-core-tests`/`junction-fx-tests`）はCIで実行されていない**（ローカル・手動実行のみが前提と読める）。`junction:test:manual`/`junction:test:integration`/`junction:test:audio`/`junction:test:network` も同様にCIワークフローには現れない。

---

## 10. 今回の再設計で影響を受けそうな既存コードの一覧

| ファイル | 影響の性質 | 変更難易度所感 |
|---|---|---|
| `native/mixxx-engine-host/src/junction/authority.h` / `.cpp` | `phase` の値集合と `authorize()`/`prepare/fence/commit/cancel/advance/recover` の意味そのものを再設計する中心。フェーダースタートでは「準備→フェンス→検証→コミット」の多段階から「フェーダーが動いた瞬間の即時切替＋事後整合」へ寄せる必要がありそう。 | 高（4章で述べたrecoveryとの結合、authorize()の拒否メッセージがUI/UXに直結） |
| `native/mixxx-engine-host/src/junction/runtime.cpp`（特に `selectLiteOwner()`, `Runtime::command("handoff.*")` 群, `tick()` 内のフェンス/検証/コミット進行ロジック） | 最大のボリューム。`selectLiteOwner()` の即時切替パスがフェーダースタートの雛形に近く、これをMac↔Mac/Lite↔Liteにも一般化する方向性が有力。 | 高（2000行の巨大 `Impl`、副作用が密結合） |
| `native/mixxx-engine-host/src/junction/junction_input.h` / `.cpp`（`TakeoverAnchor`, `InputRelease`, `JunctionInput`） | フェーダーで音を出した瞬間の検出には、既存の「レベルが可聴かどうか」（`InputRelease::observe`のaudible判定、`backend->junctionInputState()["audible"]`）が使えそう。逆方向（新DJがフェーダーを上げた瞬間の検出）の新規ロジックが必要。 | 中（既存のフェード検出ロジックの反転・流用が可能） |
| `native/mixxx-engine-host/src/junction/replay_driver.h` / `.cpp`, `mixxx_backend.cpp` の `tryCaptureGraph()`/`tryFinalizeGraphRestore()` | 演奏グラフの複製（4デッキ・DSP状態）を「ハンドオフの前提」から外せるかどうかが鍵。フェーダースタートは指名・準備フェーズを持たない可能性があり、グラフ転送のタイミング設計をゼロから見直す必要。 | 高（recoveryとの結合、Warm-Replayの排他制御を理解する必要） |
| `native/mixxx-engine-host/src/junction/program_mixer.h` | `ProgramMixerRoute::resolve()` の `localOperator` 判定（`auth.owner==auth.local`）がそのまま「誰がProgramを握るか」を決めている。フェーダースタートでも意思決定点として存続させやすい設計（コメントにもその旨の記述あり）。 | 低〜中（既に「音がどこに流れるかを決めるだけ」の純粋関数として分離済み） |
| `native/mixxx-engine-host/src/junction/session_protocol.h` / `.cpp` | `MessageType`（`HandoffRequest/Prepare/Fence/Fenced/Ready/Commit/Activate/Cancel`）と `HandoffCommitMessage` 等のワイヤ形式。新しいメッセージ種別追加または既存の意味変更が必要になる可能性が高い。 | 中（スキーマ変更は両実装（送受信）を同時に変える必要があり、後方互換もReadmeの「固定依存の扱い」方針に沿って検討要） |
| `src/services/junction/program-mixer.ts`（`junctionStage()`, `STAGE_LABEL`, `stripActions()`） | UIのステージ・アクション語彙。フェーダースタートでは `preparing/fenced` に相当するUI状態がなくなる/変わる可能性が高く、`StripAction`（request/accept/release/nominate）の再設計が必要。 | 中 |
| `src/components/junction/JunctionPanel.tsx`（`onChooseParticipant/onAcceptHandoff/onCancelHandoff/onRequestTurn`） | 「指名」「準備OK」「確定」という3操作のUIフローが不要になる可能性が高い中心コンポーネント。 | 中〜高（628行、他タブとの結合あり） |
| `src/components/play/JunctionPerformanceStrip.tsx`, `JunctionInputDeck.tsx` | LOCAL NEXTのデッキ選択UIとJUNCTION MASTERのフェーダーUI自体は再設計後も残る可能性が高いが、「フェーダーを上げたら自動でON AIR」というトリガーをこの周辺に実装する必要がある。 | 中 |
| `src/services/junction/state.ts` + `JunctionBar.tsx` の1秒ポーリング | フェーダースタート方式は「音を出した瞬間」に反応する必要があり、1秒間隔のポーリング（+コマンド起因の逐次snapshot）では体感遅延が大きい。プッシュ型の状態配信（イベント）への変更を検討する価値がある。 | 中（既存のpub/subストア自体は流用可能、配信トリガーの追加が必要） |
| `backend/mcp_server/tools/junction.py` の `junction_request_handoff/junction_accept_handoff/junction_cancel_handoff/junction_start_session` | MCP経由のAIエージェント操作も上記UIフローと同じ语彙（指名/準備/確定）に依存しており、フェーダースタート化で操作意味が変わる。 | 中 |
| `src-tauri/src/junction_mcp_bridge.rs` の `dispatch_action()` allowlist | 7章で述べた `input.set`/`input.release` の欠落を含め、新しい操作（フェーダー検出のMCP露出など）を追加する際は必ずこのallowlistも同時に変更する必要がある。 | 低〜中（allowlistパターンなので機械的だが漏れやすい） |

---

## まとめて注記した未確認事項

- `native/mixxx-engine-host/src/junction/validation.h` の `compareAudio()` の実装詳細は未読（ヘッダ宣言と使用箇所のみ確認）。
- `src/components/play/JunctionTrackList.tsx` の中身は未読（ファイル名からの推測のみ）。
- `src/hooks/useJunction.ts` / `useDdj1000.ts` の全文は未読（grep結果のみ）。
- `src/services/junction/share-musics/*`（Google連携の自動招待交換）は概要のみ確認、詳細ロジックは未読。
- `backend/mcp_server/tools/junction.py` の `_call("input.set", ...)` が実際に失敗するかどうかは、実行して確認したわけではない（コード読解上の指摘）。
