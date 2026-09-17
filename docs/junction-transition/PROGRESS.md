STATUS: DONE

## 進行中
- なし（全タスク完了）

## 次にやること
- 人手確認：`docs/junction-transition/MANUAL-CHECK.md`
- 後続の整理候補（今回は削除せず到達不能化）：Junction Live の表示用 UI（PlayLibrary の Junction Live ソース、JunctionTrackList、useJunctionTracks、PlayWorkspace のモニターデッキ経路、tracks.ts）、ネイティブの SharedTrackList・asset 転送、状態移送（グラフ書き出し・ReplayDriver 経由の復元・音声照合・フェンス）とそのレガシーテスト

## タスク一覧
- [x] T0.1 現状の把握（bda83c6）
- [x] T0.2 share-musics（PlumDeck Lite）の調査（0cf5aa6）
- [x] T0.3 設計書（70b92a9）
- [x] T1.1 状態機械（8c17e1d）
- [x] T1.2 順番の自動化（8b2ae50）
- [x] T1.3 ブースモニター（8b2ae50, d0c56e6）
- [x] T1.4 フェーダースタート（8b2ae50）
- [x] T1.5 tail（8b2ae50, d0c56e6）
- [x] T1.6 継ぎ目（ホストが受け手の場合）（8b2ae50）
- [x] T2.1 ホストが J を次のDJへ中継（8b2ae50）
- [x] T2.2 待機ストリーム・継ぎ目切替・遅延予算（8b2ae50）
- [x] T2.3 Mac↔Mac の交代と最初のDJの開始を新フローへ（8b2ae50, 321517f）
- [x] T2.4 Mac送り手からのJ曲情報（8b2ae50）
- [x] T3.1 プレイ画面（57ce024）
- [x] T3.2 Junctionパネル（bd84d01）
- [x] T3.3 DDJ-1000でのJ割り当て（46e1512）
- [x] T3.4 MCPツール・ブリッジ更新（895dc63, 321517f）
- [x] T4.1 Lite: 状態モデル・プロトコル・機能フラグ（share-musics 395d5e5, plumdeck d0c56e6）
- [x] T4.2 Lite: Jチャンネル・フェーダースタート検出・tail方針（share-musics 395d5e5）
- [x] T4.3 Lite: Mac→Lite SendRecv（既存の sendrecv を利用。Lite 受け手の J と待機ストリーム、Mac の Lite ホスト対応：share-musics 395d5e5, plumdeck d0c56e6）
- [x] T4.4 Lite: 要件書改訂（FR-28〜30、スコープ外の矛盾解消：share-musics 395d5e5）
- [x] T5.1 不要コード削除（D2 の条件に従い、状態移送とファイル転送は到達不能化＋コントラクトテスト、MCP の Junction Live は理由付きエラー：321517f）
- [x] T5.2 ドキュメント更新（4122b3e、share-musics 395d5e5）
- [x] T5.3 バージョン更新（0.5.19 → 0.6.0：e39e92e）
- [x] T5.4 全テスト実行・MANUAL-CHECK.md
- [x] T5.5 STATUS を DONE に

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
- 2026-09-17 T3.1: J は既存の JunctionInputDeck をデッキ列の先頭に置き、デッキと同じヘッダー（番号・曲名・残り時間・BPM）にした。SYNC の基準は「BPMを合わせる」（選択中デッキのテンポを J の BPM に合わせる）で提供。経路・遅延・返送は「診断」に折りたたみ。
- 2026-09-17 T3.3: DDJ-1000 の J 割り当ては CH3/CH4 から選択。割り当てたチャンネルのミキサー部（LEVEL・EQ・CUE・割り当て）だけが J を操作し、同じ側のデッキ部は元のデッキを操作する。OUTGOING 中のロック対象の操作はトーストを出さずに捨て、pickup を未取得に戻す。
- 2026-09-17 T3.4: MCP の旧 handoff ツールは名前を残し（既存クライアントの互換）、呼ぶと理由をエラーで返す。ブリッジに欠けていた input.set / input.release を追加（T0.1 の発見）。README のツール数は既に古かった（85/39 → 実際は 87/41）ので、現状（96/50）に直した。
- 2026-09-17 T4.x: share-musics の作業ブランチ feat/junction-fader-start は、Junction 実装があるブランチ feature/junction-lite-b2b から作成した（既定ブランチには Lite B2B が入っていないため）。Lite の送出音は「デッキごとのゲート＋マイク」と「受け手の J（ゲート付き）」を合わせ、MASTER つまみを通さない構造に変更。J を送出に含めるのは D2（前の DJ の音は受け手の J で会場に出る）のため。送り返しにならないのは、Lite の送り先がホストだけで、ホストはそれを Program にのみ使い、STANDBY の開始が解放の後に限られるため。Lite のデッキにはループ機能がないので、tail のデッキは全面ロック。
- 2026-09-17 T4.x: Lite ホストの ON AIR の DJ が切断した場合、READY の次の DJ がいれば交代し、いなければホスト自身に戻す（Lite には復旧状態がないため、会場を無音にしない判断）。
- 2026-09-17 T1.3/T4.x: 「受け手のマスター音量つまみで会場の音量が跳ねない」を満たすため、ネイティブの Program と J への送出を EngineMixer のメインゲイン前から取るフックを追加した（3 つのマイクモニターモードすべて）。Program メーターも同じ値を表示。
- 2026-09-17 T5.1: 状態移送（グラフ書き出し、ReplayDriver 経由の復元、音声照合、フェンス、未来フレームでの切り替え）は、ReplayDriver と mixxx_backend に深く結びつき、旧版との互換経路でもあるため削除せず、fader-start では関連メッセージ（handoff.* / graph.* / validation.* / asset.*）を受け付けないゲートで到達不能にし、コントラクトテストで固定した。recovery はこれらを使わないことも同じテストで固定。Junction Live は約 55 か所（波形・ドラッグ経路を含む）にまたがるため UI の削除は見送り（データが来ないため表示されない）、MCP ツールだけ理由付きエラーにした。
- 2026-09-17 T5.4: 手動 E2E の複数DJテストが時々「録音の冒頭で Program 無音」で落ちた。計測の結果、交代の途切れではなく、セッション開始から会場遅延（1 秒）が経つ前に録音を始めていたことが原因だった（遅延 0.5 秒のときは冒頭 0.5 秒の除外に収まっていた）。テストを「Program に音が出てから録音を始める」に修正（判定基準は弱めていない）。
- 2026-09-17 T5.3: バージョンは 0.6.0（Junction の交代に新しい機能フラグが必要になり、旧版とは交代できないため minor を上げた）。

## 検証ログ
- 基準（変更前）: `junction-core-tests` 84 passed / 4 skipped、`pnpm junction:test:ui` 55 pass、`pnpm junction:test:manual` 14 pass / 1 skip。
- T1.1: `junction-core-tests` 93 passed、`node --test src/services/junction/turn-state.test.mjs` 6 pass、`tsc --noEmit` 成功。
- T1.2〜T2.4: `junction-core-tests` 95 passed / 4 skipped。
- T1.2〜T2.4: `pnpm junction:test:manual` 15 pass / 1 skip（BlackHole 2ch 実機ループバック）。新規「fader start: a remote first DJ, guest to host and host to guest…」で、J 無音からの最初のDJ、ゲスト→ホスト、ホスト→ゲスト（中継）の交代、tail ロック表、J の曲情報（ファイル転送なし）、J を下げての解放、B2B 繰り返し、合図を実音声で確認。録音した Program に 5ms を超える無音なし。3人テストでゲスト→ゲストの J 中継も確認。
- T1.2〜T2.4: `pnpm junction:test:integration`（シグナリングサーバー経由）1 pass。承認、フェーダースタート交代、tail、シグナリング再起動、ゲスト停止からの recovery.resume を確認。
- T1.2〜T2.4: `pnpm junction:test:ui` 61 pass。
- T3.1〜T3.4: `pnpm junction:test:ui` 61 pass（turn-state・junction-channel・control-boundary・roster-model を含む）、`tsc --noEmit` 成功、`uv run pytest tests/test_mcp_junction.py tests/test_mcp_server.py` 65 pass、`cargo test junction` 6 pass。
- T4.x（share-musics）: `npm run typecheck` 成功、`npm test` 12 pass（junction-turn・junction-audio-routing）。
- T5.4（plumdeck、最終）: `junction-core-tests` 95 passed / 4 skipped、`pnpm junction:test:manual` 16 pass / 1 skip（新規の「state transfer is unreachable…」を含む）、`pnpm junction:test:integration` 1 pass、`pnpm junction:test:contract` 54 + 61 pass、`pnpm junction:test:audio` 3 passed / 1 skipped、`pnpm junction:test:network` 54 pass、`pnpm junction:test:ui` 61 pass、`tsc --noEmit` 成功、`cargo test`（src-tauri）53 passed / 4 ignored、MCP の pytest（test_mcp_junction / test_mcp_server）65 pass。
- T5.4: `pnpm test:backend`（backend 全体）は Junction と無関係の既存の失敗あり：`tests/workflow_integrity/test_light_analysis.py` の収集エラー、`tests/portable_audio/test_beat_alignment.py`（Essentia の実検出 4 件）、`tests/test_analyzer.py`（2 件）、`tests/test_grid_candidates.py`（4 件）。このブランチで変更した backend のファイルは MCP の Junction ツールとそのテストだけで、これらのテストは触れていない（要人手：解析系テストの環境・既存不具合の確認）。
- 未検証: `cross-platform-runtime.test.mjs`（Windows 実機が必要）。新方式に書き換え済みだが未実行。
- 未検証: 画面の見た目（plumdeck の Junction は Tauri とネイティブエンジンが必要、Lite は Google ログインが必要なため、ブラウザでの目視確認はしていない）。実機 DDJ-1000、2 台の Mac・別回線、スマホ実機、長時間運用も未検証。MANUAL-CHECK.md にまとめた。

## 要判断（ユーザー）
- なし

## 要人手
- `docs/junction-transition/MANUAL-CHECK.md` の各項目（実機 DDJ-1000、2 台の Mac、別回線・TURN、スマホの Lite、長時間運用、CUE の実音声、画面の見た目、Windows の cross-platform テスト、アプリ内ドキュメントのスクリーンショット撮り直し）。
- share-musics のブランチ feat/junction-fader-start（395d5e5）も push / PR が必要。
