# Junction 仕様ベースライン

engine-and-junction.md の W2-30。Rust への移植（W2-31〜W2-47）が守るべき、C++ 版 Junction（`native/mixxx-engine-host/src/junction/`）の振る舞いの基準。
設計の経緯は `docs/junction-transition/`（DESIGN.md、CURRENT.md、LITE.md）にある。ここには移植で変えてはならないことだけを書く。

同じディレクトリの JSON は `tools/junction-spec/baseline.mjs --write` がソースから生成する。

| ファイル | 中身 |
|---|---|
| `OPMAP.json` | `junction.*` コマンド（実装側、呼び出し側、読むパラメータ、拒否メッセージ）、制御メッセージとシグナリングの型、権限表、MCP ツール、各層の不一致 |
| `TESTMAP.json` | Junction を扱うテスト全件（C++、ネイティブの Node、フロント、シグナリング、MCP）と移植先・状態 |
| `CONSTANTS.json` | Junction のソースの名前付き定数 |
| `BASELINE.json` | ソース、テスト、コマンドのハッシュ |
| `PORTING-LEDGER.md` | TESTMAP.json から作る移植台帳 |

CI は検査モードを実行する。Junction のソース、テスト、コマンド、定数のどれかが変わると、追加・削除・変更の一覧を出して落ちる。そのときはこの文書と台帳を見直してから `--write` する。
この文書が `junction.*` コマンドや制御メッセージの型に触れていない場合も落ちる。

## 1. 境界と配線

- エンジンの NDJSON は `junction.<command>` を `junction::Runtime::command` に渡す（`host.cpp` が接頭辞を外す）。失敗すると `junction_rejected` エラーになり、message には日本語の理由が入る。
- セッション中は、`junction.` 以外のエンジン op もすべて `Runtime::authorize` を通る（§6）。
- Junction のネットワークプロトコルの版（`kJunctionProtocolVersion`）は、ローカルの NDJSON の `protocol=1` とは独立している。
- フロントの呼び出しは Tauri の `junction_command` が中継する（`src/services/junction/client.ts`）。MCP からは、Python のツールが `src-tauri/src/junction_mcp_bridge.rs` の HTTP ブリッジを呼び、ブリッジが action をコマンドに変換する。対応表は OPMAP.json の `mcp`。

## 2. コマンド

各コマンドのパラメータと拒否メッセージは OPMAP.json の `commands` にある。拒否メッセージは利用者に見える文言なので、移植でも一字一句変えない。

**状態と診断**
- `snapshot`：UI のスナップショット。フロントは `active` と `participants` の形を確かめてから使う。
- `input.probe`：音声プローブ。環境変数 `PLUMDECK_JUNCTION_AUDIO_PROBE` が設定されていて、セッション外のときだけ使える。JunctionOp には含まれない（試験専用）。

**ネットワーク設定**（セッションの有無に関係なく使える）
- `network.get`、`network.configure`、`network.clear`、`network.test`（`poll` と `cancel`）。
- 管理者の REST シークレットはキーチェーンにだけ保存し、スナップショットに載せない。外に出るのは期限付きでスコープを絞った TURN パスワードだけ。

**手動交換**（シグナリングサーバーを使わない）
- `exchange.inspect`：貼り付けたパケットを、信頼する前に安全な範囲だけ見せる。
- `create`（`exchangeMode:"manual"` または `signalingUrl` が空）と `join`（`PLUMDECK-JUNCTION-` で始まる招待文）。
- `invite.create`、`invite.cancel`、`invite.rotate`、`exchange.import`、`peer.approve`（`accept`）、`peer.retry`。
- セッションはホストを含めて 8 人まで。招待の有効期限は 15 分で、ICE 資格情報の期限が先に来る場合はそちらに合わせる。

**セッションの運用**
- `profile.update`、`roster.reorder`、`session.start`、`recovery.resume`、`leave`、`end`。

**交代**（フェーダースタート）
- `turn.join`、`turn.leave`、`turn.repeat`、`turn.failover`、`turn.onair`、`turn.force`、`turn.skip`、`turn.release`、`turn.cue`。

**会場出力**（ホスト）
- `program.configure`（`programDevice`、`gain`）、`program.record.start`、`program.record.stop`。

**JUNCTION 入力**
- `input.set`、`input.release`。

**プライベート試聴**（ヘッドホンだけで鳴らし、EngineMixer には入れない）
- `private.load`、`private.play`、`private.pause`、`private.stop`、`private.seek`、`private.gain`、`private.unload`、`private.state`。
- runtime は `private.` で始まる op をバックエンドの PrivatePreview に渡し、PrivatePreview は op の末尾で振り分ける。

**PlumDeck Lite**（ブラウザ版との接続）
- `lite.join`、`lite.exchange`、`lite.guest.offer`、`lite.peer.ensure`、`lite.peer.answer`、`lite.peer.remove`、`lite.owner.set`、`lite.roster.set`。

## 3. 制御チャネル（デスクトップ同士）

- 信頼性があり順序が保たれるチャネルで送るメッセージの型は 9 つ：`peer.hello`、`session.snapshot`、`clock.probe`、`clock.reply`、`session.recovery`、`health.report`、`peer.leave`、`session.end`、`turn`。
- `turn` は交代に関するメッセージで、`payload.kind`（`join`、`leave`、`onair`、`release`、`force`、`report`、`cue`、`decks` など）で種類を表す。
- エンベロープの `senderPeerId` は、そのメッセージが届いた接続の認証済み ID と一致しなければならない。ペイロードの中でホストを名乗ることはできない。
- 64 ビットのカウンタは JSON 上では 10 進数の文字列にする。符号、空白、先頭のゼロ（`"0"` 以外）、範囲外の値は拒否する。
- `kMaxControlMessageBytes` を超えるフレームは、切り詰めずに理由を付けて拒否する。デコーダは、値を状態機械に渡す前に形と範囲を検査する。
- ローカルのパス、デバイス ID、招待トークン、秘密鍵は、ワイヤーに載せる前にエンコーダが取り除く。
- 機能の交渉は `peer.hello` の `capabilities` で行う（`junction-fader-start-v1`）。相手が対応していない場合は、旧方式に黙って戻さずに更新を促す。
- Lite との DataChannel の語彙は `hello`、`owner`、`report`、`onair`、`input-released`、`tail-ended`、`cue`、`join`、`leave`、`decks`。

## 4. 手動交換のパケット

- 形式は `PLUMDECK-JUNCTION-1.<base64url>`。
- 種類は 3 つ：invite（ホスト→ゲスト。セッションの ID と、トリクルしない 2 本のオファー）、response（ゲスト→ホスト。2 本のアンサー）、notice（ホスト→ゲスト。署名付きの拒否、取り消し、期限切れ）。
- 上限は、テキスト `kMaxExchangeTextBytes`、JSON `kMaxExchangeJsonBytes`、SDP `kMaxExchangeSdpBytes`、寿命 `kMaxExchangeLifetimeMs`。
- デコードに失敗したときのコードは `too_large`、`invalid`、`unsupported_version`、`schema`、`expired`。
- ホストが出すパケットは、ホストの DTLS 秘密鍵で署名し、証明書を同梱する。ゲストは最初の招待で証明書の指紋を固定し、以後はその指紋で検証できないパケットを拒否する。
- SDP の `a=fingerprint:sha-256` は、パケットが宣言する指紋と一致しなければならない。
- 署名対象は、キーを整列したコンパクトな JSON（Qt の QJsonDocument の出力）。移植ではこのバイト列を完全に再現する（W2-33 の受け入れ基準）。
- パケットには長期の秘密を入れない。ICE 資格情報は 24 時間以内のものだけを受け付ける。

## 5. 時計と音声の経路

- 時計は 3 つあり、混ぜない：MediaTimeline（48 kHz の共有フレーム N(t)、T0 から数える）、ClockEstimator（相手の単調時計をホストの時計に写す。RTT の低い回だけを使い、ドリフトは数百 ppm に抑える）、AudioClockAnchor（デバイスのフレームを単調時計に結び付け、実際のレートを測る）。
- 壁時計（UTC）は使わない。システム時計を変えても音は動かない。
- PCM のリング（`pcm_ring.h`）は単一生産者・単一消費者。push は割り当て、ロック、ブロックをしない。溢れたら数えて捨て、バッファは伸ばさない。メタデータはペイロードと一緒に公開する。epoch、世代、レートの継ぎ目は 1 回の pop にまたがらせない。ブロック内のオフセットは `mediaFrameAdvance()` でワイヤーのフレームに直す（44.1 kHz と 48 kHz を取り違えると 8.8% ずれる）。
- JUNCTION デッキ（`junction_input.h`）は、48 kHz で届くブロックを 44.1 kHz で読む。読み手は再生中のメディアフレームを公開し、それが継ぎ目の唯一の根拠になる。目標フィルは `kTargetFill`、超過分の上限は `kMaxExcess`、継ぎ目の許容幅は `kMaxSeamLag48k` と `kMaxSeamLead48k`、補正は `kMaxAlignFrames` まで。
- Program の経路（`program_mixer.h`）：JUNCTION MASTER は前の DJ の master で、Auxiliary として入る。LOCAL NEXT は J を構造的に含まない。会場に届くのは Program Master だけ。J を main に入れたまま LOCAL NEXT を返すことはしない。
- 会場へ送る音と J への送出は、MASTER つまみ（ブース音量）より前から取る。会場出力の遅延は 1.0 秒に固定する。

## 6. 交代と権限

- 交代の状態（信号灯）は、既存の役割（owner、next、releasingPeer、rosterOrder）と計測値から `turn::derive()` で導く。真実の値を二重に持たない。
- READY でない理由は、直す順に並べた先頭の 1 件だけを示す。
- フェーダースタート：自分の音（J 以外のローカルチャンネル、サンプラー、マイク）が `kArmNanos` の間しきい値（`kThreshold`、約 -50 dBFS）を下回り、その後 `kOnsetNanos` の間上回ったときに、1 回だけ発火する。
- J はユニティ、フラット、THRU のときだけ受け付ける。それ以外は、意図的なミックス操作か、READY でない理由のどちらかとして扱う。
- 送出を終える側の DJ（OUTGOING）は、交代時に Program に乗っていたデッキ（tail）のうち、ループしか変えられない。`TailLock` のマスター系の拒否と許可は OPMAP.json の `authority`。tail デッキがすべて止まり、送出が `TailEnded::kSilentNanos` の間無音なら tail は終わる。
- 権限：セッション中でも `localOnly` と `readOnlyQuery` の op は常に通す。それ以外の op は、自分が送出中のときだけ TailLock の検査を受ける。`audio.config.set` は、owner でない DJ がマイクだけを切る場合に限って通す。

## 7. 移植の規則

- C++ のテストは同じスイート名・テスト名のまま移す。台帳では、`port.status` を `ported` にし、`port.rust` に `crate::module::fn` を書く。検査は、その関数が `src-tauri` に実在するかを確かめる。
- C++ の `junction input` と `junction-input` は同じモジュールのスイートで、名前の揺れは移植でも残す（OPMAP.json の `mismatches` に記録してある）。
- `private.stop` は runtime が受け付けるが、JunctionOp には無い（`private.pause` と同じ動き）。移植でも受け付ける。
- 定数は CONSTANTS.json の値を使う。値を変えるときは、この文書の該当箇所も直す。
- ネイティブの Node テスト（`tests/junction/*.test.mjs`）は、Rust のランタイムに対して同じ手順で通す（W2-42 と W2-45）。フロントのテストは Rust エンジンに対してそのまま再実行する（W2-47）。シグナリングサービスは TypeScript のまま残す。
