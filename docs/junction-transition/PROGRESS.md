STATUS: IN_PROGRESS

## 進行中
- タスク: T0.2
- 方針: `SHARE_MUSICS_DIR` → `../share-musics` → ホーム配下の順で PlumDeck Lite リポジトリを探し、Junction／DjPlayPage／MixerEngine／peerConnection／JunctionPanel／要件（FR-29等）／Worker API を調べて `docs/junction-transition/LITE.md` にまとめる。
- 触るファイル: docs/junction-transition/LITE.md（新規予定）, docs/junction-transition/PROGRESS.md
- 途中経過: T0.1 完了。CURRENT.md 作成済み（未コミット→今回コミット）。T0.2 はこれから着手。

## 次にやること
- T0.2 share-musics（PlumDeck Lite）の調査
- T0.3 設計書

## タスク一覧
- [x] T0.1 現状の把握
- [ ] T0.2 share-musics（PlumDeck Lite）の調査
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

## 検証ログ
- （まだ無し）

## 要判断（ユーザー）
- なし

## 要人手
- （T0.2 の結果待ち。share-musics が見つからない場合はここに記録する）
