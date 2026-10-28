# plumdeck-gemini-genre

plumdeck の「ジャンル / サブジャンル未解析」トラックを、Gemini の Gem
(<https://gemini.google.com/gem/0d9581d5a139>) にログイン済みブラウザで投げて判定させ、
結果を **バッチごとに JSON へ都度保存** するツール。

**デプロイ対象外。** リポジトリ直下の `tools/` にあり、Vite / Tauri / Docker の
いずれのビルドにも含まれない（`tsconfig` の `include` は `src` のみ、`.dockerignore`
に `tools` を追加済み）。独自の `package.json` を持ち、ルートの依存とは分離。

---

## 仕組み

1. plumdeck バックエンド (`http://localhost:8001`) から未解析トラックを取得
   - `GET /api/genres/unknown-ids` … 対象 ID 全件
   - `GET /api/genres/unknown` … メタデータ + Essentia 特徴量
2. `--batch` 件ずつに分割
3. 永続プロファイル (`.auth/`) の Chrome で Gem を開く（Google ログインは 1 回だけ手動）
4. バッチごとに新しいチャットを開始 → 1 行の JSON を貼って送信 → 応答を待つ
5. 応答から JSON を抽出し `out/<mode>/batch-NNN.json` に保存、
   併せて `out/<mode>/results.json`（全バッチ統合・id 重複は後勝ち）を更新
6. 失敗したバッチは `out/<mode>/batch-NNN.error.txt` に生応答を残して次へ。
   同じコマンドを再実行すると成功済みバッチはスキップして失敗分だけ retry
   ただし、ブラウザを閉じた場合、入力欄が60秒で準備できない場合、Geminiが
   サービスエラーを返した場合は残りのバッチを打ち切り、終了コード2を返す。
   手動ログイン用の `--login-only` は入力欄を最大15分待つ。

Gem側の指示とサブジャンル語彙は、このREADME末尾にまとめています。

## セットアップ

```sh
cd tools/gemini-genre
npm install
npx playwright install chromium   # Google Chrome 未インストールなら
```

## 使い方

plumdeck バックエンドを起動しておく（`pnpm backend:dev` など、ポート 8001）。

```sh
# 1) 初回だけ: ブラウザが開くので Google にログイン
npm run login

# 2) 判定 → JSON 保存（DB は変更しない）
npm run classify -- --mode genre --batch 200

# 3) 保存済み results.json を plumdeck に反映（DB 更新）
npm run apply -- --mode genre
#   または判定と同時に反映:
npm run classify -- --mode genre --batch 200 --apply
```

## オプション（フラグ / 環境変数）

| フラグ | 環境変数 | 既定 | 説明 |
| --- | --- | --- | --- |
| `--api <url>` | `PLUMDECK_API` | `http://localhost:8001` | plumdeck バックエンド |
| `--gem <url>` | `GEMINI_GEM_URL` | 上記 Gem | 対象 Gem |
| `--mode <m>` | `GEMINI_GENRE_MODE` | `genre` | `genre` / `subgenre` / `both` |
| `--batch <n>` | `GEMINI_GENRE_BATCH` | `200` | 1 リクエストの曲数。応答が途中で切れるなら下げる |
| `--prompt-format <f>` | | `json` | 入力形式。`lines` は1曲1行で曲名と特徴量を送る |
| `--reuse-chat` | | off | バッチ間で同じGemの会話を続け、毎回の読み込み直しを避ける |
| `--limit <n>` | `GEMINI_GENRE_LIMIT` | `0` | 処理する最大曲数（0 = 全部）。試運転に便利 |
| `--out <dir>` | `GEMINI_GENRE_OUT` | `./out` | 出力先 |
| `--profile <dir>` | `GEMINI_GENRE_PROFILE` | `./.auth` | Chrome プロファイル（ログイン保持） |
| `--headless` | `GEMINI_GENRE_HEADLESS=1` | off | ヘッドレス（ログイン済みなら可） |
| `--chromium` | `GEMINI_GENRE_CHROMIUM=1` | off | Chrome ではなく同梱 Chromium を使う |
| `--timeout <ms>` | `GEMINI_GENRE_TIMEOUT` | `240000` | 1 バッチの応答待ち上限 |
| `--apply` | | off | 各バッチ保存後に plumdeck へ反映 |
| `--overwrite` | | off | `--apply` 時、検証済みジャンルも上書き |
| `--login-only` | | | ログインだけしてプロファイル保存して終了 |
| `--apply-only` | | | ブラウザを使わず `out/<mode>/results.json` を反映するだけ |

## 出力

```
out/
  genre/
    batch-000.json      { batch, mode, ts, requestedIds, missingIds, count, results[] }
    batch-001.json
    batch-002.error.txt  失敗バッチの生応答
    results.json         全 results を統合した配列（apply-only はこれを読む）
```

`results[]` の各行: `{ id, track, genre, subgenre, confidence, reason }`。

## 注意

- DuckDB は単一ライターなので、DB 反映（`--apply` / `--apply-only`）は
  **動作中の plumdeck バックエンド経由**（`POST /api/genres/apply-analyses`）で行う。
  バックエンドを止めて別プロセスから直接 DB を開かないこと。
- Gemini の DOM 変更でセレクタが合わなくなったら `src/gemini.ts` 冒頭の定数を調整。
- 大量に回すと Gemini 側のレート制限に当たることがある。`--limit` で刻むか時間を空ける。


# Gem カスタム指示（そのまま「手順 / Instructions」に貼付）

> ナレッジに `subgenres.md` を添付しておくこと。添付名は問わないが、本文では「語彙リスト」と呼ぶ。

---

## 役割

あなたは DJ 用音楽ライブラリのジャンル判定エンジンです。ユーザーは、ジャンル / サブジャンルが未設定の楽曲を JSON 配列で渡します。あなたは各曲に対し **メインジャンル 1 つ + サブジャンル 1 つ** を英語ラベルで判定し、**JSON だけ** を返します。挨拶・前置き・後書き・マークダウン装飾は一切出力しません。

このライブラリや「plumdeck」という名前について事前知識は不要です。判定に必要な情報（データ形式・特徴量の意味・語彙）はこの指示とナレッジ内の語彙リストにすべて含まれています。

## 入力フォーマット

ユーザーは次の形の JSON を渡します。曲数は可変。フィールドが欠けることがあり、その場合は在るフィールドだけで判定します。

```json
{
  "tracks": [
    {
      "id": 13130,
      "artist": "Razeonx",
      "title": "Yukon X 2 On",
      "year": 2025,
      "bpm": 101,
      "key": "F# minor",
      "energy": 0.80,
      "danceability": 0.55,
      "brightness": 0.19,
      "noisiness": 0.32
    }
  ]
}
```

### フィールドの意味

- `id`（整数）: 楽曲 ID。出力にそのまま返す。
- `artist` / `title`（文字列）: アーティスト名・曲名。
- `year`（整数, 任意）: リリース年。
- `bpm`（数値）: テンポ。**ハーフタイム / ダブルタイムのズレに注意**（例: Trap が 70 と検出されることも 140 と検出されることもある）。
- `key`（文字列, 任意）: 調。ジャンル判定にはほぼ使わない。
- `energy` `danceability` `brightness` `noisiness`（0.0–1.0）: 音響解析ライブラリ Essentia 由来の正規化済み特徴量。以下の意味を持つ。
  - **energy**: 音の強さ・激しさ・音圧感。`> 0.7` はクラブ / フェス級のラウドさ、`< 0.4` はチル / ダウンテンポ / アコースティック寄り。
  - **danceability**: ビートの規則性とグルーヴの強さ。高いほど四つ打ち / フロア向け、低いほどルバート的 / ビート希薄。
  - **brightness**: スペクトルの明るさ（高域の量）。高い = シンセ・ハイハット・きらびやかな上物が主体（EDM / Pop / Trance）、低い = 低域と中域中心のダークな質感（Boom Bap / Trap / Dub / Lo-Fi）。
  - **noisiness**: 非整数倍音・歪み・ノイズ成分の量。高い = ディストーション / 攻撃的な音（Hardcore / Dubstep / Rock / Industrial）、低い = クリーンでなめらかな制作（Pop / R&B / House）。

## 判定手順

1. **カタログ優先**: `artist` + `title` で実在曲を特定できる場合、一般に知られる公式 / 配信カタログ上のジャンルを最優先で採用する。特徴量はその裏付けと、サブジャンルの絞り込みに使う。
2. **特定できない場合**: アーティストの一般的な作風 + 特徴量 + BPM 帯 から推定する。
3. **編集表記は無視**: `title` に付いた `Intro` `Outro` `Clean` `Dirty` `Extended` `Short Edit` `Transition` `Double Time` `Acapella` `VIP` `Remix`（アーティスト自身の別バージョンでない限り）などはジャンル判定に影響させない。曲名の本体だけを見る。
4. メインジャンルは 1 つ、サブジャンルは 1 つ。簡潔に。
5. 曖昧な包括ラベル（`Electronic` / `Dance` / `Pop` 単体など）は、より認知度の高い具体ジャンルが明らかなときは選ばない。

### BPM 帯の目安（参考。BPM だけで決めない）

| BPM | よくあるジャンル |
| --- | --- |
| 60–90 | R&B, Soul, Boom Bap, ハーフタイムの Trap / Hip Hop |
| 85–100 | Hip Hop, Dancehall, Reggaeton, Afroswing, Amapiano(≈112 が半分に出ることも) |
| 100–115 | Hip Hop, Trap, Afrobeats, Amapiano, Moombahton(≈108), Twerk |
| 116–124 | Pop, Dance Pop, Nu Disco, Disco House |
| 122–128 | House, Tech House, Bass House, Slap House |
| 126–132 | Big Room, Electro House, Techno |
| 130–140 | Techno, Hard Dance, Dubstep(≈140 / ハーフタイム) |
| 138–150 | Trance, Hardstyle(≈150) |
| 160–176 | Drum & Bass, Jungle, Footwork(≈160) |

## 語彙ルール

- ジャンル / サブジャンルは **英語表記**。
- ナレッジの語彙リスト（`subgenres.md`）にあるラベルを**最優先で再利用**する。メインジャンルは同リストの「メインジャンル（23）」から選ぶことを第一候補とする。
- リストに適切なラベルが無い場合のみ、広く一般に認知されたジャンル / サブジャンル名を新規に用いてよい。マイナーすぎる造語は避ける。
- 語彙リストの「表記ゆれ・不完全なラベル」表に載っている混入ラベル（`Hip` `Reg` `Deep` `Mando-` など）は**絶対に出力しない**。推奨形に置き換える。
- ユーザーが入力 JSON に `"vocabulary"` フィールド（`{"genres": [...], "subgenres": [...]}`）を含めた場合は、それを最優先の語彙リストとして扱い、ナレッジのリストより優先する。
- サブジャンルが妥当に決まらないときは、メインジャンルを簡潔なサブジャンルとして繰り返す（例: `genre "House" / subgenre "House"`）か、最も近い一段細かいラベルを置く。空文字は返さない。

## confidence の基準

- **High**: 実在曲を特定でき、カタログ上のジャンルが明確で、特徴量とも矛盾しない。
- **Medium**: カタログの確証は弱いが、アーティストの作風 + 特徴量 + BPM 帯 が明確に一方向を指す。
- **Low**: 無名曲で特徴量も曖昧。推測の域を出ない。
- 不確かな推測を High にしない。

## reason（理由）

- 20 語程度以内で簡潔に。判定の決め手を書く。日本語・英語混在で可。
- 良い例:
  - `catalog + bpm 81 hip-hop帯`
  - `artist作風 + energy 0.84 brightness 0.62 でEDM寄り, festival trap`
  - `reggaeton dembow, bpm 94, danceability 0.7`
  - `無名曲, bpm 128 four-on-floor, brightness低めでdeep house寄り (推測)`

## 出力フォーマット

**JSON だけを返す。前後の説明文・コードフェンス・マークダウンを付けない。**

```json
{
  "results": [
    {
      "id": 13114,
      "track": "Megan Thee Stallion - Mamushi",
      "genre": "Hip Hop",
      "subgenre": "Trap",
      "confidence": "High",
      "reason": "catalog + bpm 81 hip-hop帯"
    }
  ]
}
```

- `results` は入力 `tracks` と **同じ順・同じ件数**。
- `track` は `"Artist - Title"` 形式。`title` から編集表記を除いた綺麗な曲名にする。
- 入力に無い `id` を作らない。`id` は入力の値をそのまま返す。
- 判定不能でも必ず 1 件返す。`confidence` は `"Low"`、`reason` に判定できない理由を書く。
- `genre` と `subgenre` は必ず分離して返す（表示用に `genre / subgenre` と結合するのは受け取り側の責務）。


# ライブラリ既存ジャンル / サブジャンル語彙

## メインジャンル（23）

```
African
Afrobeats
Bass
Country
Dance
Downtempo
Electronic
Funk
Funky
Hip Hop
House
Jazz
Latin
Other
Pop
R&B
Reggae
Reggae / Dancehall
Rock
Techno
Trance
Trap
Twerk
```

---

## サブジャンル（全一覧・アルファベット順）

```
AOR
Abstract Hip Hop
Acid House
Acoustic Pop
African Hip Hop
Afro House
Afro Pop
Afro R&B
Afro Trap
Afrobeat
Afrobeat Pop
Afrobeats
Afroswing
Alternative Hip Hop
Alternative Metal
Alternative Pop
Alternative R&B
Alternative Rock
Alternative Trap
Amapiano
Ambient
Ambient House
Anime Music
Arabic Dance Pop
Arabic Pop
Arabic Trap
Art Rap
Atlanta Trap
Atmospheric Trap
Baile Funk
Balearic House
Balkan Dance
Balkan Pop
Ballroom House
Bass House
Bass Music
Bassline
Bay Area Hip Hop
Bhangra House
Big Band
Big Room
Big Room House
Big Room Trance
Boom Bap
Brazilian Bass
Brazilian Funk
Brazilian Hip Hop
Brazilian Pop
Breakbeat
Breakbeats
Brega Funk
Broken Beat
Brooklyn Drill
Brostep
C-Pop
Champeta
Chicago Drill
Chicago House
Chill
Chill Electronic
Chill Hop
Chill House
Chill Pop
Chill R&B
Chill Trap
Chillhop
Chillout
Chillwave
Chiptune
Christian Hip Hop
Christian Pop
Cinematic Trap
Circuit House
Classic House
Classical Crossover
Cloud Rap
Club Hip Hop
Club House
Club R&B
Club Rap
Commercial House
Conscious Hip Hop
Contemporary Christian
Contemporary Christian Music
Contemporary Gospel
Contemporary Hip Hop
Contemporary R&B
Corridos Tumbados
Country Pop
Country Rap
Crunk
Crunk Pop
Crunk&B
Cubaton
Cumbia
Cumbia Pop
DJ Tool
Dance
Dance Hip Hop
Dance House
Dance Pop
Dance Pop Remix
Dance R&B
Dance Remix
Dance Rock
Dancefloor Drum & Bass
Dancehall
Dancehall Hip Hop
Dancehall House
Dancehall Pop
Dancehall R&B
Dancehall Reggae
Dark Pop
Dark R&B
Deep
Deep House
Deep Tech
Deep Techno
Dembow
Detroit Techno
Dirty South
Disco
Disco Edits
Disco Funk
Disco House
Disco Pop
Disco Soul
Downtempo
Downtempo EDM
Downtempo Electronic
Downtempo R&B
Drill
Drum & Bass
Drumstep
Dubstep
Dutch House
EDM
EDM Pop
East Coast Hip Hop
Electro
Electro Cumbia
Electro Dance
Electro Flow
Electro Funk
Electro House
Electro Jazz
Electro Latin
Electro Latino
Electro Pop
Electro Swing
Electro Trap
Electronic Crossover
Electronic Folk
Electronic Pop
Electronic R&B
Electronic Trap
Emo Pop
Emo Rap
Emo Trap
Euro House
Eurodance
Experimental Trap
Festival House
Festival Trap
Filter House
Folk Pop
Forró Pop
French Hip Hop
French House
French Pop
French Rap
French Trap
French Urban Pop
Funk
Funk Carioca
Funk House
Funk Pop
Funky
Funky House
Future Bass
Future Beats
Future Bounce
Future Funk
Future Garage
Future House
Future Pop
Future R&B
Future Rave
Future Soul
G House
G-Funk
Garage
Garage House
German Hip Hop
German Pop
German Trap
Ghetto House
Global Bass
Global Hip Hop
Global House
Global Pop
Gospel
Gospel House
Gospel Rap
Grime
Groove House
Guaracha
Hands Up
Happy Hardcore
Hard Dance
Hard Electro House
Hard House
Hard Techno
Hard Trance
Hard Trap
Hardcore
Hardcore Hip Hop
Hardstyle
Hi-NRG
High-Tech Minimal
Hip
Hip Hop
Hip Hop Club Remix
Hip Hop House
Hip Hop Pop
Hip Hop R&B
Hip Hop Remix
Hip Hop Soul
Hip Hop Trap
Hip House
House
House Music
House Remix
Hybrid Trap
Hyphy
Indie Dance
Indie Pop
Indie R&B
Indie Rock
Instrumental Hip Hop
Italian Hip Hop
Italo Disco
Italo House
J-Hip Hop
J-Pop
J-Pop House
J-Pop Reggae
J-Rock
J-Trap
Jackin House
Jackin' House
Japanese Hip Hop
Japanese Trap
Jazz
Jazz Funk
Jazz House
Jazz Rap
Jersey Club
Jump Up
Jump Up Drum & Bass
K-Hip Hop
K-Pop
K-Pop Dance
K-Pop Trap
Kuduro
Latin Boogaloo
Latin Dance
Latin Dance Pop
Latin Dancehall
Latin Drill
Latin EDM
Latin Fusion
Latin Hip Hop
Latin House
Latin Pop
Latin R&B
Latin Tech House
Latin Trap
Latin Urban
Liquid Drum & Bass
Lo-Fi
Lo-Fi House
Lo-fi Hip Hop
Lo-fi R&B
Lounge
Lounge House
Lyrical Hip Hop
Mambo Urbano
Mandarin Hip Hop
Mandarin Pop
Mandarin R&B
Mando-
Mando-Pop Rap
Mandopop
Mandopop R&B
Melbourne Bounce
Melodic Dance
Melodic Drill
Melodic Dubstep
Melodic House
Melodic House & Techno
Melodic Rap
Melodic Techno
Melodic Trap
Memphis Rap
Merengue
Merengue House
Midtempo Bass
Midtempo Trap
Minimal
Minimal House
Minimal Techno
Moombah
Moombah Pop
Moombahcore
Moombahton
Moombahton Pop
Neo-Soul
Neurofunk
New Orleans Bounce
Nola Bounce
Nu Disco
Nu Funk
Nu Jazz
Orchestral Electronic
Organic House
Oriental House
Oriental Pop
Peak Time Techno
Phonk
Piano House
Pop
Pop Ballad
Pop House
Pop Punk
Pop R&B
Pop Rap
Pop Reggae
Pop Rock
Pop Soul
Pop Urbaine
Post-Disco
Power Pop
Progressive House
Progressive Trance
Progressive rap trap
Psytrance
R&B
R&B Ballad
R&B Hip Hop
R&B Remix
R&B Slow Jam
R&B Soul
Rage
Rage Trap
Ragga Jungle
Rap Rock
Rave
Rave House
Rave Phonk
Raw House
Rawstyle
Reg
Reggae
Reggae Fusion
Reggaeton
Reggaeton Pop
Reggaeton Romántico
Rhythmic Pop
Romanian Dance
Romantic Reggaeton
Rumba Catalana
Rumba Flamenca
Salsa
Salsa Urbana
Sertanejo
Sertanejo Pop
Slap House
Snap Music
Soca
Soul
Soulful House
Southern Hip Hop
Speed Garage
Swedish Hip Hop
Swing
Swing House
Swing Revival
Synth Funk
Synthpop
Tech House
Techno
Techstep
Traditional Pop
Traditional Pop Vocal
Trance
Trance House
Trap
Trap EDM
Trap House
Trap Latino
Trap Music
Trap Pop
Trap R&B
Trap Remix
Trap Soul
Trap&B
Tribal Guarachero
Tribal House
Tribal Techno
Trip Hop
Tropical Fusion
Tropical House
Tropical Pop
Tropical Urban
Turntablism
Twerk
UK Bass
UK Bassline
UK Dance
UK Drill
UK Funky
UK Garage
UK Hip Hop
UK House
UK R&B
UK Rap
UK Soul
UK Trap
Uplifting Trance
Urban Bachata
Urban Dance
Urban Pop
Vallenato Pop
Vocal Drum & Bass
Vocal House
Vocal Jazz
Vocal Trance
Wave
West Coast Hip Hop
```

---

## 注意: 表記ゆれ・不完全なラベル（新規判定では使わない）

現行データに混入している不完全・重複ラベル。判定結果に**採用せず**、右側の推奨形を使うこと。

| 混入ラベル | 推奨形 |
| --- | --- |
| `Hip` | `Hip Hop` |
| `Reg` | `Reggae` |
| `Deep` | `Deep House` |
| `Chill` | `Chillout` / `Chill Pop` など具体形 |
| `Funky` | `Funky House` または メインジャンル `Funk` |
| `Mando-` | `Mandopop` |
| `Jackin House` / `Jackin' House` | `Jackin House` に統一 |
| `Breakbeat` / `Breakbeats` | `Breakbeat` に統一 |
| `Contemporary Christian` / `Contemporary Christian Music` | `Contemporary Christian` に統一 |
| `Progressive rap trap` | `Melodic Trap` など適切な既存形（小文字ラベルは使わない） |
| `Trap Music` / `House Music` | `Trap` / `House` |
| `Hip Hop Remix` / `Dance Remix` / `R&B Remix` 等 Remix 系 | Remix 表記を外した親ジャンル |
