# Junction 現状調査（PlumDeck Lite = share-musics 側）

調査日: 2026-09-17。対象リポジトリ `/Users/horiyuuta/Workspace/share-musics`（ブランチ `feature/junction-lite-b2b`、調査時点で `git status` clean）。本ドキュメントはコードの実読に基づく。読み切れなかった箇所・未確認の推測は明示する。**share-musics 側では一切変更・コミット・ブランチ操作を行っていない（読み取りのみ）。**

リポジトリは存在し、Junction関連実装も存在する（「見つからない」ケースには該当しない）。

## 主要ファイル一覧（実際に grep/読み込みで確認したパス）

- `frontend/src/features/junction/api.ts` — Worker REST APIクライアント
- `frontend/src/features/junction/peerConnection.ts` — WebRTC接続クラス `JunctionPeerConnection`
- `frontend/src/features/junction/components/JunctionPanel.tsx`（576行） — Junction UI・接続オーケストレーション
- `frontend/src/features/dj-engine/MixerEngine.ts`（405行） — Web AudioミキサーエンジンとJunctionオーディオ経路
- `frontend/src/features/dj-engine/types.ts` — `JunctionMixerSnapshot`, `JunctionInputAssign` 等の型
- `frontend/src/features/dj-play/components/DjPlayPage.tsx`（312行） — DJプレイ画面本体（`junctionDecks()` 関数を含む）
- `frontend/src/features/dj-play/components/PlayWorkspace.tsx` — レイアウトラッパー（Junction固有ロジックはコメント1箇所のみ）
- `worker/src/junction.ts`（382行） — Cloudflare Worker側のセッション・シグナリングDB操作
- `worker/src/index.ts` — action allowlistとルーティング
- `docs/requirements.md`（104行） — 要件定義書（FR-25〜FR-30がJunction該当）
- `docs/design.md`（266行） — 設計書（8章がJunction p2p連携設計）
- `frontend/tests/junction-audio-routing.test.mjs`（81行） — Junctionオーディオ経路の構造テスト
- `README.md` — 一部Junction関連の記述あり（後述、**コードと矛盾する古い記述を含む**）

---

## 1. 現在のJunction状態管理

### 1.1 サーバー側（Turso、`worker/src/junction.ts`）

3テーブル構成（`migrate()` 内で定義、`worker/src/junction.ts:69-128`）:

- `junction_sessions`: `id, name, host_email, host_peer_id, owner_peer_id, status('open'|'closed'), created_at, updated_at, expires_at(24時間), host_seen_at`
- `junction_members`: `session_id, peer_id, user_email, device, client('lite'|'desktop'), display_name, role('host'|'guest'), status('pending'|'approved'|'rejected'|'left'), created_at, updated_at`。主キーは`(session_id, peer_id)`、`UNIQUE(session_id, user_email, device)`。
- `junction_signals`: `id, session_id, sender_peer_id, recipient_peer_id, kind('offer'|'answer'|'invite'|'response'|'notice'), payload_json, created_at, expires_at`。offer/answerは10分、invite/response/noticeは20分で失効。

状態遷移を起こす関数（すべて`worker/src/junction.ts`にエクスポートされ、`worker/src/index.ts:437-468`でaction名にマッピングされる）:

- `createJunctionSession()`: 同一端末が既にホスト中のセッションを`closed`にしてから新規`open`セッションを作成。作成者は`role='host', status='approved'`で自動登録。
- `joinJunctionSession()`: `role='guest', status='pending'`で登録。過去に`rejected`ならアカウント単位で再申請不可（`JUNCTION_REJECTED`）。`left`から復帰する場合は`pending`に戻す。定員は端末単位で3、`pending+approved`合計で`MAX_MEMBERS=8`。
- `approveJunctionMember()`: ホストのみ、`pending→approved|rejected`。
- `setJunctionOwner()`: ホストのみ、`junction_sessions.owner_peer_id`を書き換えるだけ（サーバー側にはowner切替の検証ロジック・フェンス・epoch概念は一切ない）。
- `leaveJunctionSession()`: ホストなら`status='closed'`＋シグナル全削除。ゲストなら`status='left'`、自分がownerだった場合は`owner_peer_id`を`host_peer_id`へ戻す。

**重要**: サーバー側は「セッション参加管理とSDP受け渡し」のみを担い、plumdeckの`Authority`（`phase`, `epoch`, `cutoverFrame`等）に相当する状態機械は存在しない。owner切替は単なる1カラムの書き換えであり、フェンス・検証・コミットの概念がゼロ。

### 1.2 クライアント側（`JunctionPanel.tsx`）

Reactの`useState`ではなく`useRef<JunctionRuntime | null>`（`JunctionPanel.tsx:29-60, 397-399`）でミュータブルに保持される構造体`JunctionRuntime`が実質的な状態機械の本体:

```
sessionId, peerId, role('host'|'guest'), hostPeerId, ownerPeerId,
afterSignalId, stopped, polling, pollTimer/announceTimer/statsTimer/releaseTimer,
hostPeers: Map<peerId, JunctionPeerConnection>,   // ホストのみ、承認済みゲストごとに1本
guestPeer: JunctionPeerConnection | null,          // ゲストのみ、ホストへの1本
remoteStreams: Map<peerId, MediaStream>,           // ホストが受信した各ゲストの音声
senderPeerIds: string[] | null,                    // DataChannelで通知された「送出中peer一覧」(null=旧ホスト互換)
ownerFromHost: boolean,                             // DataChannel由来のownerをWorkerポーリング値より優先するか
hostStream: MediaStream | null,                     // ゲストが受信したホスト音声
operating: boolean,                                 // ゲスト: 自分がownerか
inputReleased: boolean,
outgoingPeerId: string | null,                      // ホスト: 引き継ぎ中の前任ゲスト
hostOutgoing: boolean,                              // ホスト自身が前任として送出中か
ownerRevision: number,                              // 世代カウンタ(切替中の巻き戻り防止)
```

**plumdeckの`phase`（`playing/preparing/fenced/committed/switching/recovery`）に相当する概念はLite側に存在しない。** owner切替は`switchHostOwner()`（`JunctionPanel.tsx:176-193`）が呼ばれた瞬間に`current.ownerPeerId`を直接書き換える即時遷移のみで、「準備中」「フェンス中」に相当する中間状態がない。これは plumdeck の `selectLiteOwner()`（フェンスなし即時切替、CURRENT.md 1.2節参照）と設計思想が一致しており、**Lite側はそもそも最初からフェーダースタート方式に近い即時切替しか実装していない**。

DataChannel（`junction.control`, ordered:true, `peerConnection.ts:72`）で送受信される制御メッセージ（`JunctionPanel.tsx`のonControlハンドラで判別、`JunctionPanel.tsx:67-74`、`231-244`、`301-312`）:

| type | 方向 | 内容 |
|---|---|---|
| `owner` | ホスト→ゲスト | `{ ownerPeerId, senderPeerIds }`。`ownerMessage()`(`JunctionPanel.tsx:81-86`)が構築、接続確立時と毎`broadcastOwner()`呼び出し時に送信。 |
| `handoff-request` | ゲスト→ホスト | 「演奏を希望」ボタン押下時。ホスト側`handoffRequests: Set<peerId>`に追加されるだけで、順番待ちキュー化はされない。 |
| `input-released` | ゲスト→ホスト | ownerが解放ボタン/自動解放条件（後述）で送信。ホストはこれを受けて前任の送出停止・`broadcastOwner()`を実行。 |
| `decks` | ゲスト→ホスト(送出中のみ) | 500ms間隔(`announceTimer`, `JunctionPanel.tsx:402-405`)。デッキ・トラックのテレメトリ。**現状のLiteコード内に受信側の処理(`message.type === "decks"`のハンドラ)は存在しない**（grep確認、送信のみで未消費）。 |

状態遷移トリガーまとめ:
- `create()`/`join()` → Worker API呼び出し → `begin()`で`JunctionRuntime`初期化 → `pollRef.current()`即時実行
- ポーリング(`pollRef.current`, `JunctionPanel.tsx:349-395`)がWorkerから最新`session/me/signals`を取得し、`switchHostOwner()`・`handleGuestOffer()`・`handleHostMember()`をトリガー
- UIボタン: `approve()`, `setOwner()`, `releaseInput()`, `leave()`

---

## 2. 音声経路（Web Audio、`MixerEngine.ts`）

plumdeckの「JUNCTION MASTER = Mixxxの`[Auxiliary1]`（`EngineAux`）」に相当する概念はLite側にも**存在するが、ネイティブAudioエンジンではなく純粋なWeb Audio GainNodeチェーン**として実装されている。

- **JUNCTION受信経路（JUNCTION MASTER相当）**: `junctionInputBus → junctionInputLevel → junctionInputCross → junctionInputMaster → localMainBus`（`MixerEngine.ts:120-124`）。`localMainBus`は`context.destination`（ローカル出力）にのみ接続され、**`mediaDestination`（WebRTC送信先）には物理的に経路が存在しない**（`junction-audio-routing.test.mjs`がこれを正規表現で検証している）。plumdeckのリングバッファ＋48kHz→44.1kHzリサンプルに相当する処理はない（WebRTC/Opusのデコード出力をそのまま`MediaStreamAudioSourceNode`で使うため、リサンプル層は不要）。
- **送出ゲート**: `junctionSendGain`（`masterNode → junctionSendGain → mediaDestination`, `MixerEngine.ts:113-115`）。`setJunctionSending(on)`がフェード(`JUNCTION_FADE_SECONDS=0.02s`)付きで0/1切替。plumdeckの「送出中(sending)」フラグに相当。
- **LOCAL NEXT（post-fader local channel、JUNCTION自身とマイクを除外）に直接対応する概念はLite側にない**。Lite自体が2デッキ(A/B)のみを扱う設計で、plumdeckの4デッキ構成のような「これから演奏するローカルデッキ群」を別バスに分離する必要がないため。
- **Program Master（会場出力）相当**: `hostProgramBus`（`context.destination`へ直結, `MixerEngine.ts:116-117`）。`setJunctionHostProgram(stream, localOwner)`（`MixerEngine.ts:308-319`）が`localOwner`のとき`localOutputGain`をフェードイン（ローカルMASTERをそのまま出力）、そうでなければ`remote`(ゲストownerのストリーム)を`hostProgramBus`へアタッチしてフェード。plumdeckの`ProgramMixerRoute::resolve()`（`VenueSource::LocalMix|RemoteHost`の二択部分）と概念的に完全に対応するが、`DirectStream`/`None`に相当する分岐はない(常にLocalMixかRemoteHostのどちらか)。
- **フェード定数**: `JUNCTION_FADE_SECONDS = 0.02`（タップの付け外し）、`JUNCTION_DETACH_MS = 200`（タップ解放までの遅延）。plumdeckの`fenceFrame`/`cutoverFrame`のようなオーディオフレーム単位の厳密な継ぎ目管理はなく、すべて時間ベースの`AudioParam.setTargetAtTime`フェードで滑らかさを担保している。
- **フィードバック防止**: `applyGuestAudio()`のコメント（`JunctionPanel.tsx:126`）「受信音声は自分がownerの間だけMASTERへ混ぜる。前任として送出中に混ぜると相手へ送り返してループする」— plumdeckの`feedbackBlocked`ガード（`program_mixer.h:73-77`）と同じ懸念を、`operating`フラグと`hasPreviousSender`チェックで実装している。
- MONITOR CUE(`filter.connect(monitor).connect(monitorDestination)`)はJunction経路と完全に分離されており、FR-17/FR-29の「MONITOR CUEはJunctionへ含めない」を満たす。

---

## 3. peerConnection の実装

`frontend/src/features/junction/peerConnection.ts`の`JunctionPeerConnection`クラスが1接続=1インスタンス。

- **トポロジー**: **ホストをハブとするスター型**。ホストは承認済みゲストごとに`RTCPeerConnection`を1本ずつ張る（`JunctionPanel.tsx`の`hostPeers: Map<peerId, JunctionPeerConnection>`）。ゲスト同士が直接つながることはない。ゲスト↔ゲストの音声中継はホストが`applyHostReturns()`（`JunctionPanel.tsx:159-170`）で「新オーナーへのみ前任ゲストの`remoteStreams`を`replaceOutgoingStream()`で流し込む」形で実現しており、plumdeckの`returnRelaySource/returnRelayTarget`（Lite↔Lite中継、CURRENT.md 3章）と**概念的に完全に一致**する（ただしLite側はこれをネイティブC++ではなく、既存WebRTC接続の`RTCRtpSender.replaceTrack()`の使い回しだけで実現している——新しいPeerConnectionを張り直さない）。
- **シグナリング方式**: non-trickle（`waitForIce()`, `peerConnection.ts:22-30`でICE gathering完了を待ってからSDPを1回だけWorker経由で送る）。plumdeckのdesign.md 8章に明記された方針と一致。オファー送信はホストが承認済みゲストを検出した時点(`handleHostMember`, `JunctionPanel.tsx:220-286`)、アンサーはゲストがオファーを受信した時点(`handleGuestOffer`, `JunctionPanel.tsx:288-336`)。ICEサーバーは`stun:stun.l.google.com:19302`のみ（TURNなし）。
- **オーディオm-line**: 1本のsendrecv m-lineを両者で共有（`peerConnection.ts:56-105`のコメントに詳細）。オファー側は`createOffer()`前に`addTrack()`、アンサー側は`setRemoteDescription(offer)`後に`addTrack()`——順序を間違えると別トランシーバーになりRTPが流れないという実装上の注意点がコメントされている。
- **Opusステレオ強制**: `enableOpusStereo()`（`peerConnection.ts:5-20`）がSDPを正規表現操作して`stereo=1;sprop-stereo=1`を注入。plumdeckデスクトップがステレオOpusを送ってくる前提のコメントあり（「Desktop encodes stereo Opus」）——**desktop-Lite間接続を見越した実装が既に入っている**。
- **`RTCDataChannel("junction.control")`**: 制御メッセージ専用（1章参照）。`send()`は`bufferedAmount > 65_536`でドロップ（バックプレッシャー対策）。
- **`replaceOutgoingStream(stream|null)`**: m-lineを再ネゴシエーションせずに送信ソースだけ切り替える（`peerConnection.ts:114-123`）。owner切替の音声側はこれ一本で完結しており、offer/answerの再送は発生しない——**フェーダースタート方式が求める「即座の切替」に既にフィットしている設計**。
- Worker側(`postJunctionSignal`, `worker/src/junction.ts:317-353`)は`kind`によって送信方向を強制する:「offer/invite/noticeはホスト→ゲスト」「answer/responseはゲスト→ホスト」。**Liteが送受信できるのは`offer`/`answer`だけ**（`api.ts:44`の型定義、コメント「Liteが扱うのはoffer/answerだけ。invite/response/noticeはデスクトップ同士の交換で、Liteには届かない」）。`invite`/`response`/`notice`は署名付きテキスト(`PACKET_PATTERN = /^PLUMDECK-JUNCTION-1\.[A-Za-z0-9_-]+={0,2}$/`)で、双方が`client==='desktop'`でない限りサーバーが`JUNCTION_FORBIDDEN`を返す（`worker/src/junction.ts:336`）。

---

## 4. UI（`JunctionPanel.tsx`）

- **参加フロー**: 未接続時は`refreshSessions()`で`listJunctionSessions`を取得し、開催中セッション一覧をボタンで表示（`JunctionPanel.tsx:526-530`）。**招待URL・offer/answer文字列の人手交換は一切ない**（FR-25どおり、`junctionApi.list/join`で完結）。ただし`session.hostClient === "desktop"`のセッションは`DESKTOP_HOST_READY`定数（現在`true`固定、`JunctionPanel.tsx:63`）で参加可否を制御しており、コード上は既にデスクトップホストへの参加を許可する分岐が実装済み。
- **承認**: ホストのみ`pending`メンバーに対し承認/拒否ボタン(`approve()`, `JunctionPanel.tsx:467-473`)。
- **owner切替**: ホストのみ、承認済み・接続中(`connected`)のメンバーへ「演奏を任せる」ボタン(`setOwner()`, `JunctionPanel.tsx:474-497`)。**順番の自動化・キュー(plumdeckの`rosterOrder`/`finishedOrder`/`turnRequests`相当)は一切ない**——ホストは毎回手動で任意のpeerを選ぶだけ。「演奏を希望」ボタン(`handoff-request`送信)はホストの`handoffRequests: Set<peerId>`にバッジ表示されるのみで、自動指名や順序付けには繋がらない。
- **JUNCTION IN ストリップ**（`JunctionPanel.tsx:508-516`）: LEVELスライダー、L/THRU/Rクロスフェーダー割り当てボタン、「解放」ボタン。LEVEL 0が1.5秒(`AUTO_RELEASE_MS = 1_500`)続くと`releaseInput()`が自動発火（`JunctionPanel.tsx:421-425`のuseEffect）——**plumdeckの`InputRelease`（「可聴だった後1.5秒無音」でフェード検出）と定数まで一致する直接対応物**。
- **接続状況表示**: メンバーごとにconnected/pending/approvedを丸バッジで表示、`RX/TX KB`の帯域統計(`getMediaStats()`, 1秒間隔`statsTimer`)。「ON AIR」ラベル、「演奏希望」ラベル。
- **ロック連動**: `onControlLockChange`コールバックで、送出中(前任として音を出し続けている間)は`DjPlayPage.tsx`側の`junctionControlsLocked`が立ち、デッキ・ミキサー操作を無効化する（`junctionControlsLocked`は`DjPlayPage.tsx`内で17箇所参照）。
- **表示先の分岐**: 通常はドロップダウン(`sheetHost === null`)、横画面スマホの圧縮レイアウトでは`createPortal`でモーダルシートとして表示（`JunctionPanel.tsx:558-573`）。
- plumdeckの`JunctionPanel.tsx`（628行、DJ一覧/招待交換/プロフィール編集/ネットワーク設定/Google連携をタブ内包）と比べると、**Liteの`JunctionPanel`は単一の平坦なパネルで、招待交換フローとプロフィール/ネットワーク設定タブが存在しない**（Worker側が自動化しているため不要）。

---

## 5. 要件定義書（`docs/requirements.md`）該当箇所

「5.6 p2p(Junction)連携」節、FR-25〜FR-30（**FR-22は欠番——21から23に飛んでおり、番号の抜けが要件書内にある。誤記かリネームの残骸かは未確認**）:

> FR-25: ログイン済みユーザーは開催中セッションの一覧から参加でき、招待URL・offer・answer文字列を人手で交換しない。
> FR-26: ホストは参加申請を承認／拒否でき、同時参加者はホストを含め最大5人とする。
> FR-27: Cloudflare WorkerとTursoはセッション管理およびWebRTC SDP交換にだけ使用し、接続後の音声・演奏制御・デッキメタデータはWebRTCの音声トラック／DataChannelでp2p送信する。
> FR-28: ホストは承認済みの接続中peerへ演奏権(owner)を切り替えられる。ゲストは演奏希望を送信できる。
> FR-29: 演奏を操作するownerは常に1人とする。ホスト自身がownerならローカルミックス、ゲストがownerならそのpeerから受信した音声を会場出力する。交代直後の前任DJの音声は、新しいownerのJUNCTIONチャンネル（LEVEL・クロスフェーダー割り当て）で解放されるまで鳴り続けてよく、新しいownerがフェードアウトして解放した時点で前任の送出を止める。MONITOR CUEはJunctionへ含めない。
> FR-30: PlumDeckデスクトップ版とはLite方式の双方向音声とJUNCTION交代に対応する。デスクトップ固有の曲データ転送と録音管理はLite側の対象外とする。

**注意（矛盾の発見）**: 同じ`requirements.md`の「7. スコープ外(v1)」に「5. PlumDeckデスクトップ版とのJunction互換性」が明記されている（`docs/requirements.md:92`）。これは**FR-30と直接矛盾する**（FR-30は明示的にデスクトップ対応を要件化している）。実装コード側は`JunctionMember.client`型、`worker/src/junction.ts`のデスクトップ/Lite区別ロジック、`JunctionPanel.tsx`の`DESKTOP_HOST_READY`定数、`peerConnection.ts`のOpusステレオ強制コメントなど、**FR-30寄り（デスクトップ対応を前提とした実装）が既にかなり進んでいる**。スコープ外記述の方が古い/更新漏れの可能性が高いが、断定はできない（未確認）。

「8. 前提・制約」に「Junction音声リレーは『ある瞬間ちょうど1peer(owner)のMASTERのみがホストの会場出力へ中継される』設計とする」との明記あり（`docs/requirements.md:98`）。

`docs/design.md`の8章（`Junction p2p連携設計`, 211-221行目）も本ドキュメントの2〜4章の内容の設計側記述として一致している。

---

## 6. Worker API（`worker/src/index.ts` / `worker/src/junction.ts`）

`JUNCTION_ACTIONS`（`worker/src/index.ts:19`）としてallowlist登録されているaction名（すべて`/api/<action>`経由）:

```
listJunctionSessions, getJunctionSession, createJunctionSession, joinJunctionSession,
approveJunctionMember, postJunctionSignal, setJunctionOwner, leaveJunctionSession
```

うち書き込み系(`WRITE_ACTIONS`, POST限定)は`createJunctionSession/joinJunctionSession/approveJunctionMember/postJunctionSignal/setJunctionOwner/leaveJunctionSession`。`listJunctionSessions`/`getJunctionSession`はGET。ロール制御は`JUNCTION_ROLES = Set(['admin','partner','player'])`（`worker/src/index.ts:30`、`partner_lite`ロールはJunction不可）。

**`decks`という名前のWorker APIアクションは存在しない。** ユーザーから示唆のあった「`decks`というAPI名」は、Worker REST APIではなく**WebRTC DataChannel上のメッセージ型のフィールド名**として存在する（`{ type: "decks", decks: [...], at: Date.now() }`、`JunctionPanel.tsx:404`）。中身は`DjPlayPage.tsx`の`junctionDecks()`関数（`DjPlayPage.tsx:41-46`）が生成する配列で、各要素の構造:

```ts
{
  role: "current" | "next",   // 最も可聴なデッキがcurrent、他はnext
  deck: "A" | "B",
  assetId: string,
  sizeBytes: string,          // 数値を文字列化して送信
  title: string,
  artist: string,
  musicalKey: "",             // 常に空文字（未実装のプレースホルダ）
  durationMs: number,
  bpm: number,
  positionMs: number,
  rate: number,                // tempoRatio
  audibility: number,          // fader × crossfaderゲイン
  playing: boolean,
  firstBeatMs: number,
  beatsPerBar: number,
}
```

500ms間隔（`announceTimer`）で「送出中のゲスト」からホストへ一方向に送られるが、**受信側（ホスト）でこのメッセージを処理するコードはLite側に存在しない**（grep確認、消費コードなし）。おそらくplumdeckデスクトップがホストになった場合に「JUNCTIONデッキ」として表示するための布石と推測されるが、確証はない（未確認）。design.mdの「Junction `TrackAnnounce`/`SessionSnapshot`送信」（126行目）という記述がこの`decks`メッセージに相当すると思われるが、名称が完全一致しないため断定は避ける。

Worker側のエラーコードマッピング（`worker/src/index.ts:488-492`）: `JUNCTION_NOT_FOUND`(404), `JUNCTION_FULL`(409, メッセージは「8人で満員」だが実装上は`joinJunctionSession`内で端末単位3人・全体8人の二重制限), `JUNCTION_REJECTED`(403), `JUNCTION_FORBIDDEN`(403), `JUNCTION_INVALID_SIGNAL`(400)。

---

## 7. plumdeck本体（Mac/Mixxx, CURRENT.md）との対応・差分

| plumdeck概念 | Lite側の対応 | 差分・所感 |
|---|---|---|
| `Authority.phase`(`playing/preparing/fenced/committed/switching/recovery`) | 存在しない。`ownerPeerId`の直接書き換えのみ | Lite側は元から中間フェーズなし＝**フェーダースタート方式に最初から近い**。plumdeckのフェンス/検証/コミットを新方式で簡略化する際、Liteの単純な「即時owner切替＋DataChannel通知」がむしろ参考実装になる。 |
| `epoch`（音声ブロックの世代不整合防止） | 存在しない | Lite側は`RTCRtpSender.replaceTrack()`とオーディオのフェードだけで新旧を繋いでおり、フレーム単位の世代管理はない。新方式でepoch概念を導入するなら、Lite側にも同等のタグ付けを追加する必要が生じる可能性がある。 |
| `selectLiteOwner()`（Mac側、Lite関連ハンドオフの即時切替） | `switchHostOwner()` + `setJunctionOwner` API + `owner`DataChannel通知 | 概念的に対になる実装。**ただし相互のワイヤフォーマット互換性は本調査では未確認**（`selectLiteOwner()`が実際にどのメッセージをLiteへ送るか、CURRENT.mdには記載があるが本調査ではplumdeck側media_transport.cpp/session_protocol.hの該当箇所までは読んでいない）。T4.3「Lite: Mac→Lite SendRecv」で要突合。 |
| JUNCTION MASTER(`[Auxiliary1]`, `EngineAux`) | `junctionInputBus→…→junctionInputMaster→localMainBus`（Web Audio GainNodeチェーン） | 概念は一致。ネイティブのリングバッファ/サンプルレート変換は不要（WebRTC/Opusのデコード後ストリームをそのまま使うため）。 |
| LOCAL NEXT(`AudioBridge::localReturn`、post-fader・JUNCTION+マイク除外バス) | 対応する専用バスなし | Liteは2デッキのみでこの分離が不要という設計判断だが、新方式で「次に演奏するデッキ」を明示的に扱う場合は新設が必要になる可能性。 |
| Program Master(`ProgramMixerRoute::resolve()`, LocalMix/DirectStream/RemoteHost/None) | `setJunctionHostProgram(stream, localOwner)`（LocalMix/RemoteHostの二値相当） | DirectStream/Noneに相当する分岐がなく単純。ホスト=plumdeck・ゲスト=Liteのケースでは、この関数がそのままMac側のProgram Master切替と対になるはず（詳細な対応表は未確認）。 |
| `InputRelease`（1.5秒無音／2秒断／5分フォールバックで前任解放） | `AUTO_RELEASE_MS=1_500` + `releaseTimer`(`JunctionPanel.tsx:187-192`, 300_000ms=5分のセーフティタイムアウト) | **1.5秒・5分の定数まで一致**。ストリーム断2秒に相当する明示的なタイマーはLite側では見当たらず、`onState`の`failed/closed`イベント（即時）で代替している（未確認: plumdeckの2秒判定とLiteの即時判定のどちらがより新方式に適するか要検討）。 |
| `seamFrame`/`TakeoverAnchor`（フレーム精度の継ぎ目） | なし。`JUNCTION_FADE_SECONDS=0.02s`/`JUNCTION_DETACH_MS=200ms`の時間ベースフェードのみ | Web Audioでフレーム精度の継ぎ目管理をする場合は`AudioWorklet`等への作り替えが必要になりうる（大掛かりな変更）。 |
| `returnRelaySource`/`returnRelayTarget`（Lite↔Lite中継） | `applyHostReturns()`（`JunctionPanel.tsx:159-170`）が同じ役割 | 概念だけでなく実装粒度もほぼ一致（ホストが中継ハブ）。 |
| `feedbackBlocked`ガード | `operating`/`hasPreviousSender`チェック(`applyGuestAudio`) | 同じ懸念への対処。 |
| Mac↔Mac検証(`compareAudio`のPCM相関チェック) | なし | Lite関連の交代はそもそもフェンス・検証を経由しないため対応不要（plumdeckのLite分岐と同じ扱い）。 |
| 演奏グラフ転送(4デッキ/DSP/サンプラー状態、`ReplayDriver`) | なし。デッキの再生内容(トラック)自体は交代してもpeer間で転送されない | **新方式で「状態移送を通常フローから外す」場合、Lite側にはそもそも移送する状態がない**——`decks`メッセージはメタデータの一方向通知のみで、実際の音源・再生位置の同期・転送は行われていない。ホストのデッキとゲストのデッキは完全に独立。 |
| Recovery(`backupOwner`/`backupEpoch`、音声レベルのフォールバック) | なし。`onState`の`failed`/`closed`で`applyHostProgram(current, current.ownerPeerId)`を再評価するだけ | 障害時の明示的な「recoveryフェーズ」はなく、単に接続断イベントに対する素朴な後処理のみ。新方式でrecoveryを明文化するなら、Lite側にも同等のイベントフックが必要。 |
| `JunctionBar`の1秒定周期ポーリング | Liteは可変間隔（未接続/接続処理中1秒、安定後ホスト10秒・ゲスト30秒、`JunctionPanel.tsx:388`） | **ただし実際のowner切替通知はポーリングではなくDataChannelの`owner`メッセージによる即時プッシュ**（`broadcastOwner()`はポーリングと独立に呼ばれる）。ポーリング間隔が影響するのは主に「参加申請の検知」「offer/answer信号の到達」であり、切替そのものの体感遅延はplumdeckのポーリングボトルネック（CURRENT.md 6章）ほど深刻ではない可能性がある。 |
| MIDI(DDJ-1000)Junction割り当て | 該当なし（ブラウザ、物理コントローラ非対応） | スコープ外のまま。 |
| MCPツール(41個、`junction.py`) | 該当なし（ブラウザアプリ、MCPサーバーなし） | Lite側にAIエージェント操作の余地はコードレベルで存在しない。 |
| requirements.md FR-30 vs スコープ外記述の矛盾 | （5章参照） | plumdeck↔Lite統合を進める上で、どちらが正か確認が必要。実装は明らかにFR-30寄り。 |

---

## 8. テストの実行方法

- **`frontend/package.json`のscriptsに`test`は定義されていない**（`dev`/`build`/`typecheck`/`preview`のみ）。`worker/package.json`にも`test`スクリプトなし（`dev`/`deploy`/`typecheck`のみ）。**ルートに`package.json`自体が存在しない**。
- Junction関連の唯一のテストファイルは`frontend/tests/junction-audio-routing.test.mjs`（node:testベース）。実行するには`npm test`のようなラッパーが無いため、直接次のように呼ぶ必要があると推測される（**package.jsonにscript化されていないため、実際の実行コマンドは開発者の暗黙知/READMEに依存しており、リポジトリ内には明記が見当たらない＝未確認**）:
  ```
  node --test frontend/tests/junction-audio-routing.test.mjs
  ```
  （TypeScriptの`.tsx`/`.ts`を`readFile`で文字列として読み込み、正規表現でソースコードの構造・不変条件をアサートする「契約テスト」形式——実際にブラウザでWebRTC/Web Audioを動かす実行テストではない。）
- **CI設定ファイル（`.github/workflows/*`等）はリポジトリ内に見当たらない**（`find`で`.yml`/`.yaml`を検索したが0件）。つまりこのテストは少なくとも自動化されたCIでは実行されていないと考えられる（未確認だが根拠あり）。
- 型チェックは`frontend`で`npm run typecheck`（`tsc -b --pretty false`）、`worker`で`npm run typecheck`（`tsc --noEmit`）。
- README.mdにはJunction専用の手動テスト手順の記載は見当たらなかった（プールモード/プレイモード全般の運用手順はあるが、Junctionの実機確認手順は明記なし）。

---

## 9. 新方式（フェーダースタート方式）でLite側に影響が大きいと思われる箇所

1. **owner切替のトリガー方式そのもの**: 現状は`setOwner()`というホストの明示的なボタン操作でのみ切り替わる（`JunctionPanel.tsx:474-497`）。「フェーダースタート」＝フェーダーを上げた瞬間に自動で切り替わる方式にするなら、`MixerEngine`側でチャンネルフェーダー/オーディビリティのしきい値検出ロジックを新設し、それを`JunctionPanel`のowner切替トリガーへ配線し直す必要がある。現在フェーダー値を監視しているのは`junctionEffectiveLevel`（JUNCTION INチャンネルのLEVEL、解放判定にのみ使用）だけで、**A/B本線デッキのフェーダー操作を owner 獲得のトリガーにする仕組みは存在しない**。
2. **tail送出の明文化**: 現在の「前任DJの音を新ownerが解放するまで鳴らし続けてよい」動作(`FR-29`)は実装上`outgoingPeerId`/`hostOutgoing`という状態と`AUTO_RELEASE_MS=1500`のタイムアウトだけで実現された副次的な挙動であり、「tail」という明示的な設計概念・パラメータ（尺の長さ、フェード形状など）としては存在しない。新方式で tail を独立した設計要素にするなら、この暗黙の挙動を明示的なAPI（例: `setTailPolicy()`のような）へリファクタリングする必要がある。
3. **状態機械の欠如そのもの**: plumdeckの`Authority`に相当する型・enumがLiteには一切ないため、新方式で仮に「共通の状態モデル」をMac/Lite間で共有する設計にする場合、Lite側は**ゼロから状態モデルを実装する**ことになる（PROGRESS.mdのT4.1「Lite: 状態モデル・プロトコル・機能フラグ」がこれに対応すると見られる）。
4. **`decks`メッセージの仕様未確定**: 現状送信専用・未消費のメッセージであり、新方式での「Mac送り手からのJ曲情報」（PROGRESS.md T2.4）とどう統合するかの仕様がLite側には存在しない。名称・フィールド構成（`musicalKey`が常に空文字など未完成な形跡あり）を新方式のプロトコルに合わせて再定義する必要が高い。
5. **`DESKTOP_HOST_READY`と要件矛盾**: 実装は「Macがホスト、Liteがゲスト」のケースを既に許可する形跡があるが（5章参照）、要件定義書はスコープ外と矛盾した記載を残しており、新方式の設計に着手する前に要件側の整合を取る必要がある（PROGRESS.md T4.4「Lite: 要件書改訂（FR-29）」のタイミングで一緒に解消するのが妥当と思われる）。
6. **ポーリング設計とDataChannelプッシュの役割分担の明文化**: 現状owner切替通知自体はDataChannelの即時プッシュで行われており大きなボトルネックではなさそうだが、参加承認・offer/answer信号はWorkerポーリング(最短1秒間隔)に依存している。新方式が「最初の接続確立」も含めた全体の即応性を要求するなら、この部分の設計変更（WebSocket化、Durable Objects化等）が必要になる可能性がある(現状のCloudflare Worker + Turso REST方式のままで十分かは未検討)。
7. **フレーム精度の継ぎ目がない**: plumdeckの`seamFrame`/`TakeoverAnchor`のようなオーディオフレーム単位の精密な継ぎ目管理がLiteには存在せず、すべて時間ベースのフェード(20ms/200ms)で代替している。新方式がフレーム精度の一致を要求する仕様になった場合、Web Audioの`AudioParam`スケジューリングやWorkletでの精密化が必要になり、実装難易度は高い。
8. **`applyHostReturns()`の中継ロジックの一般化**: 現状は「新ownerだけが前任ゲストの音を受け取る」という1系統の中継しか想定しておらず(`JunctionPanel.tsx:159-170`)、複数の同時中継や、host自身がゲストから引き継ぐ場合とゲスト間引き継ぎの場合とで分岐が複雑（`hostOutgoing`と`outgoingPeerId`の二重管理）。新方式で交代パターンが増える場合、このロジックの整理が必要になりそう。
9. **`JunctionPeerConnection`の1接続=1トラックという制約**: 現状は1本のRTCPeerConnectionに音声track1本のsendrecv m-lineという単純な構成。新方式で「複数ソースの同時送出」や「Mac側の2本PeerConnection構成(`pc[0]`control用/`pc[1]`bulk転送用)」に類する複雑化が必要になった場合、Lite側の`peerConnection.ts`の設計をほぼ作り直すことになる（現状はplumdeckのMac↔Mac方式のような`MediaTransport`の2PC構成に対応していない、ブラウザ互換の1PC限定=`MediaTransport::startLite()`と対になる設計のまま）。

---

## まとめて注記した未確認事項

- `decks`メッセージ(DataChannel)の受信側実装がLiteに存在しない理由（未実装なのか、plumdeckデスクトップ側だけが消費する設計なのか）は未確認。
- plumdeckの`selectLiteOwner()`が実際にLite側の`owner`DataChannelメッセージと同じワイヤフォーマットで通信しているかどうかは、本調査ではplumdeck側の`media_transport.cpp`/`session_protocol.h`を再確認しておらず未確認（CURRENT.mdの記述をベースにした推測）。
- README.mdの「Junctionはplumdeckが発行した`plumdeck-junction://join?...`招待URLをそのまま貼り付ける」「`VITE_JUNCTION_SIGNALING_URL`」という記述は、実装コード全体をgrepしても該当箇所が一切見つからず（`VITE_JUNCTION_SIGNALING_URL`、`plumdeck-junction://`ともに0件）、**README側が実装より古い、または別方式の名残の可能性が高い**（断定はしない）。
- `frontend/tests/junction-audio-routing.test.mjs`の正確な実行コマンド（npmスクリプト化されていない）は開発者の運用に依存しており、リポジトリ内に明記がないため未確認。
- requirements.md の FR-22 欠番の理由は未確認。
- FR-30とスコープ外(v1)の矛盾について、どちらが正しい現在の方針かは未確認（コード実装はFR-30寄り）。
