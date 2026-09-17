STATUS: IN_PROGRESS

## 進行中
- タスク: T3.1
- 方針: プレイ画面を `snapshot.turn` に合わせる。信号灯と案内1行、J をデッキ列の1デッキとして表示、LOCAL NEXT のデッキ選択と Junction Live カードの廃止、次のDJの準備状況、合図、OUTGOING のロック表示、診断の折りたたみ。
- 触るファイル: src/components/play/JunctionPerformanceStrip.tsx, JunctionInputDeck.tsx, JunctionTrackList.tsx, PlayWorkspace.tsx, src/services/junction/program-mixer.ts, src/services/performance-command-router.ts ほか
- 途中経過: ネイティブ側（T1.1〜T2.4）は完了・コミット済み（8b2ae50）。UI はこれから。

## 次にやること
- T3.1 プレイ画面
- T3.2 Junction パネル
- T3.3 DDJ-1000
- T3.4 MCP

## タスク一覧
- [x] T0.1 現状の把握（bda83c6）
- [x] T0.2 share-musics（PlumDeck Lite）の調査（0cf5aa6）
- [x] T0.3 設計書（70b92a9）
- [x] T1.1 状態機械（8c17e1d）
- [x] T1.2 順番の自動化（8b2ae50）
- [x] T1.3 ブースモニター（8b2ae50）
- [x] T1.4 フェーダースタート（8b2ae50）
- [x] T1.5 tail（8b2ae50）
- [x] T1.6 継ぎ目（ホストが受け手の場合）（8b2ae50）
- [x] T2.1 ホストが J を次のDJへ中継（8b2ae50）
- [x] T2.2 待機ストリーム・継ぎ目切替・遅延予算（8b2ae50）
- [x] T2.3 Mac↔Mac の交代と最初のDJの開始を新フローへ（8b2ae50）
- [x] T2.4 Mac送り手からのJ曲情報（8b2ae50）
- [ ] T3.1 プレイ画面
- [ ] T3.2 Junctionパネル
- [ ] T3.3 DDJ-1000でのJ割り当て
- [ ] T3.4 MCPツール・ブリッジ更新
- [ ] T4.1 Lite: 状態モデル・プロトコル・機能フラグ
- [ ] T4.2 Lite: Jチャンネル・フェーダースタート検出・tail方針
- [ ] T4.3 Lite: Mac→Lite SendRecv
- [ ] T4.4 Lite: 要件書改訂（FR-29）
- [ ] T5.1 不要コード削除
- [ ] T5.2 ドキュメント更新
- [ ] T5.3 バージョン更新
- [ ] T5.4 全テスト実行・MANUAL-CHECK.md
- [ ] T5.5 STATUS を DONE に

## 判断ログ
- 2026-09-17: PROGRESS.md が存在しなかったため新規作成。既定ブランチ main から feat/junction-fader-start を作成して着手。
- 2026-09-17: ユーザーから「矛盾がなければ最後まで作りきる。分割報告の指示は無視してよい」と指示があったため、1セッション1〜3タスクの制限を外して連続で進める。
- 2026-09-17 T0.1: 指示のパス（`cmake/target/CMakeLists.txt` 等）は実際と一部相違。実在パスに読み替え（CURRENT.md 冒頭）。
- 2026-09-17 T0.1: `selectLiteOwner()` が既にフェンスなし即時切替を持ち、フェーダースタートの雛形になる。状態移送と recovery は `Authority` の `phase`/`epoch`/`cutoverFrame` を共有しているので、`phase` の値集合は変えない。
- 2026-09-17 T0.2: Lite 側に Authority 相当の状態機械はなく、`switchHostOwner()` の即時切替のみ。`AUTO_RELEASE_MS=1500` はネイティブの 1.5 秒解放と一致。`decks` は DataChannel メッセージで受信側が未実装。share-musics の requirements.md に FR-30 とスコープ外の矛盾あり（T4.4 で整合）。
- 2026-09-17 T1.1: 状態は既存の役割（owner/next/releasingPeer/rosterOrder/finishedOrder）から導出するだけにし、新しい真実を持たない。`turnPhase` フィールドを足す設計書案はやめ、`turn::derive()` の純粋関数にした（判断基準4）。
- 2026-09-17 T1.2: 順番は rosterOrder（常に全員を含む）と finishedOrder（列から外れた人）で表す。B2B の繰り返しは `turnRepeat` フラグで、交代した人を finished に入れないことで実現（重複登録はしない）。turnRequests は「順番に入る」に置き換え。
- 2026-09-17 T1.4: ON AIR の合図は LOCAL NEXT バス（J とマイクを構造的に除外）のコールバック単位ピーク＋マイクON＋Jの等倍からの変化。閾値 -50dBFS、無音300ms で待機、有音20ms で発火。定数は turn_state.h の1か所で、TS とテストで一致を検査する。
- 2026-09-17 T1.4/T2.3: fader-start モードの `Authority::authorize` は epoch 不一致・fence・recovery で拒否しない。交代の瞬間に epoch が進んでも、演奏中のDJのフェーダー操作が拒否されないようにするため（判断基準2）。拒否は OUTGOING の tail ロックだけ。旧 fence 方式のテストは互換のため残した。
- 2026-09-17 T1.5: tail の送出は送り手側で LOCAL NEXT バスをデッキのマスク＋切替時点のマスター音量で固定したもの（EngineMixer フックに `localReturnMask`/`localReturnFixedGain` を追加）。ゲストは OUTGOING を知った時点（スナップショット、最大約0.5秒後）で tail を確定する。この間に新しく鳴らしたデッキは含まれる可能性があるが、実害は小さいので許容。
- 2026-09-17 T2.2: 継ぎ目は、次のDJが自分のキャプチャを J の再生フレームに合わせ直した最初のブロック（既存 TakeoverAnchor と同じ仕組み）で決め、そのフレームとキャプチャ世代をホストへ送る。ホストはその世代以降だけを Program に使う。待機ストリームは次の epoch（`standbyEpoch`）で送る。
- 2026-09-17 T2.2: 会場出力の遅延は 0.5 秒から 1.0 秒に固定変更した。ゲストが受け手のとき J のバッファ（約120ms）＋Opus 受信保持（60ms）＋回線で 250ms の予算を超え、LAN でも READY にならなかったため。本番中に遅延を変えない決定事項に合わせ、STANDBY 中に自動で動かすのではなく、測定値が予算（遅延の半分＝500ms）を超えたら READY にしない方式にした。
- 2026-09-17 T2.3: 状態移送（グラフ書き出し・音声照合・フェンス）のコードは削除せず、通常フローから到達しない状態にした（handoff.* 操作は理由付きで拒否、HandoffPrepare を送る経路なし）。recovery は独立経路のまま E2E で動作確認済み。削除は T5.1 で判断。
- 2026-09-17 T2.4: Junction Live の音源ファイル転送は fader-start では無効（announceTracks と TrackAnnounce 受信を止めた）。曲情報は `turn` の decks メッセージ（パスなし、500ms 間隔）で送り、ホストが受け手へ中継。表示位置は J の遅延ぶん補正。
- 2026-09-17 T2.x: PlumDeck Lite はフレーム精度の継ぎ目を持たないため、Lite が絡む ON AIR は時刻ベース（既存の即時切替と同じ）。Lite は DataChannel の `hello{capabilities}` で機能フラグを示す（T4.1 で実装）。
- 2026-09-17 T3.x 前: J 等倍でない理由の文言を「初期位置に戻しています」から「等倍・EQフラット・THRUに戻してください」に変更（DJ が自分で戻す必要があるため）。

## 検証ログ
- 基準（変更前）: `junction-core-tests` 84 passed / 4 skipped、`pnpm junction:test:ui` 55 pass、`pnpm junction:test:manual` 14 pass / 1 skip。
- T1.1: `junction-core-tests` 93 passed、`node --test src/services/junction/turn-state.test.mjs` 6 pass、`tsc --noEmit` 成功。
- T1.2〜T2.4: `junction-core-tests` 95 passed / 4 skipped。
- T1.2〜T2.4: `pnpm junction:test:manual` 15 pass / 1 skip（BlackHole 2ch 実機ループバック）。新規「fader start: a remote first DJ, guest to host and host to guest…」で、J 無音からの最初のDJ、ゲスト→ホスト、ホスト→ゲスト（中継）の交代、tail ロック表、J の曲情報（ファイル転送なし）、J を下げての解放、B2B 繰り返し、合図を実音声で確認。録音した Program に 5ms を超える無音なし。3人テストでゲスト→ゲストの J 中継も確認。
- T1.2〜T2.4: `pnpm junction:test:integration`（シグナリングサーバー経由）1 pass。承認、フェーダースタート交代、tail、シグナリング再起動、ゲスト停止からの recovery.resume を確認。
- T1.2〜T2.4: `pnpm junction:test:ui` 61 pass。
- 未検証: `cross-platform-runtime.test.mjs`（Windows 実機が必要）。新方式に書き換え済みだが未実行。

## 要判断（ユーザー）
- なし

## 要人手
- Windows 実機での cross-platform テスト（T5.4 の MANUAL-CHECK.md に記載予定）。
