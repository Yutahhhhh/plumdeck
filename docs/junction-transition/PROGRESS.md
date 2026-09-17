STATUS: IN_PROGRESS

## 進行中
- なし。T0.1〜T0.3（フェーズ0：調査と設計）を完了し、本セッションはここで終了する（1回の実行の上限3タスクに達したため）。
- 次回セッションが T1.1 に着手する際の方針（参考）: `TurnState`（OFF/STANDBY/READY/ON AIR/OUTGOING、DESIGN.md 1.1〜1.2節）を純粋関数として実装し、C++ユニットテスト（junction-core-tests）を追加する。TS側（program-mixer.ts）も同じ導出にしてテストする。触るファイル想定: native/mixxx-engine-host/src/junction/authority.h/.cpp（turnPhase/readyBlockers追加）、新規 turn_state.h/.cpp、tests/junction/turn_state_test.cpp、src/services/junction/program-mixer.ts。

## 次にやること
- T1.1 状態機械
- T1.2 順番の自動化

## タスク一覧
- [x] T0.1 現状の把握
- [x] T0.2 share-musics（PlumDeck Lite）の調査（share-musics は `/Users/horiyuuta/Workspace/share-musics` で発見。LITE.md 作成済み）
- [x] T0.3 設計書（DESIGN.md 作成、D1〜D4との矛盾なしを確認）
- [ ] T0.3 設計書
- [ ] T1.1 状態機械
- [ ] T1.2 順番の自動化
- [ ] T1.3 ブースモニター
- [ ] T1.4 フェーダースタート
- [ ] T1.5 tail
- [ ] T1.6 継ぎ目（ホストが受け手の場合）
- [ ] T2.1 ホストが J を次のDJへ中継
- [ ] T2.2 待機ストリーム・継ぎ目切替・遅延予算
- [ ] T2.3 Mac↔Mac の交代と最初のDJの開始を新フローへ
- [ ] T2.4 Mac送り手からのJ曲情報
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
- 2026-09-17 T0.1: 指示にあったファイルパス（`cmake/target/CMakeLists.txt` 等）は実際と一部相違していたため、実在するパスに読み替えて調査した。詳細は CURRENT.md 冒頭の注記を参照。
- 2026-09-17 T0.1 重要発見（設計判断に直結）:
  1. `runtime.cpp` の `selectLiteOwner()`（662-698行）が、Lite絡みのハンドオフに限りフェンスなしの即時オーナー切替を既に実装している。フェーダースタート方式の最も近い既存テンプレートとして T1.1/T1.4 で参照する。
  2. 状態移送（ReplayDriverのgraph/DSPカプセル）と音声レベルの復旧（recovery）は実装としては分離しているが、`Authority` の `phase`/`epoch`/`cutoverFrame` を共有しているため結合している。`phase` の値を新フロー用に変更すると、`runtime.cpp:1469` の `route()` 内の recovery `allowed()` 判定を壊すリスクがある。T2.3 で状態移送を切り離す際は必ずこの共有フィールドへの影響を先に確認する。
  3. MCPの `input.set`/`input.release` 相当の操作が `junction_mcp_bridge.rs` の許可リストに対応する腕（arm）が見当たらない疑い（未実行確認）。T3.4 で要再確認。
  4. ネイティブ側 `junction-core-tests`/`junction-fx-tests` と `junction:test:manual`/`integration`/`audio`/`network` はCIで実行されていない。手動実行が前提。T5.4 の全テスト実行時に注意。
- 2026-09-17 T0.2 重要発見:
  1. Lite側には plumdeck の `Authority.phase` 相当の状態機械が無く、`JunctionPanel.tsx` の `switchHostOwner()` が `ownerPeerId` を直接書き換える即時遷移のみ。plumdeck の `selectLiteOwner()`（フェンスなし即時切替）と設計思想が一致しており、Lite は元々フェーダースタート方式に近い。T1.1/T4.1 の設計で有利に働く。
  2. Lite の `AUTO_RELEASE_MS=1500` は plumdeck の「可聴だった後1.5秒無音で解放」（D3の解放条件）と定数まで一致する既存の対応物。D3実装はLite側の値をそのまま踏襲できる。
  3. `decks` という名前のメッセージは Worker API ではなく DataChannel 上のメッセージ型（`DjPlayPage.tsx` の `junctionDecks()` が生成、500ms間隔）。受信側の消費コードがLite内に存在せず未使用/布石と推測。T2.4/T4.2 で仕様を確定させる必要あり。
  4. share-musics の `docs/requirements.md` に要件矛盾を発見：FR-30（デスクトップ版とのJunction交代対応）と「7.スコープ外(v1)」の「PlumDeckデスクトップ版とのJunction互換性」が直接矛盾。コード実装はFR-30寄りに進んでいる。T4.4 の要件書改訂時に整合を取る（D1〜D4とは矛盾しないため要判断には計上せず、作業ルールに従い進行可能なタスクとして処理）。
  5. share-musics の README.md に実装と一致しない記述（`plumdeck-junction://` 招待URL、`VITE_JUNCTION_SIGNALING_URL`）を発見。コード全体をgrepしても該当0件。古い記述の可能性。T5.2 のドキュメント更新時に併せて確認・修正する。
  6. Lite側にフレーム精度の継ぎ目（seamFrame/TakeoverAnchor相当）は無く、時間ベースのフェード（20ms/200ms）のみ。T2.2/T4.2 の継ぎ目設計で、Web Audio特有の制約として考慮する。
- 2026-09-17 T0.3: `Authority.phase` の既存値集合は変更せず、新フィールド `turnPhase`（off/standby/ready/onair/outgoing）を独立軸として追加する方針にした。理由: `phase` を変更すると recovery の `route()` 内 `allowed()` 判定（T0.1発見の結合リスク）を壊す恐れがあるため（判断基準4「状態の二重管理を作らない」と一見矛盾するように見えるが、「既存の真実は変えず、UI向けの導出軸を1つ追加する」ことで二重管理を避ける設計とした）。
- 2026-09-17 T0.3: B2Bの繰り返し表現方法、Mac側解放タイマーの正確な値、`decks`メッセージの正式スキーマは未決のままT1.2/T1.5/T2.4へ引き継ぐ（進行を止めるほどの不確実性ではないため要判断には計上しない）。

## 検証ログ
- （まだ無し）

## 要判断（ユーザー）
- なし

## 要人手
- （T0.2 の結果待ち。share-musics が見つからない場合はここに記録する）
