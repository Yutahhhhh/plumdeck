"""Whole-set planning for MCP agents.

The agent reads the DJ's rekordbox folders or interviews the DJ about the
night, turns that into ordered phases, and plan_phased_setlist picks and orders
real library tracks for every phase. plumdeck does not reason about the story
of the set; the agent calling these tools does.
"""
from typing import Any, Dict, List, Optional

from mcp_server.instance import mcp, db_session
from app.services.set_planner_service import SetPlannerService
from infra.rekordbox_library import RekordboxLibraryUnavailable


def _rekordbox(call):
    try:
        return call()
    except RekordboxLibraryUnavailable as error:
        raise ValueError(str(error)) from error


@mcp.tool()
def list_rekordbox_playlists(under: Optional[str] = None, depth: int = 2) -> Dict[str, Any]:
    """rekordbox のプレイリスト/フォルダ階層を読み取り専用で返す。
    under にフォルダの ID かパス（例: '現場物' や '現場物/イベ2'）を渡すとその配下だけを返す。
    depth は返す階層の深さ（1〜6）。各項目の path は plan_phased_setlist の
    rekordbox_playlists にそのまま渡せる。インテリジェントプレイリスト(kind=smart)は読み込めない。"""
    if not 1 <= depth <= 6:
        raise ValueError("depth must be 1..6")
    with db_session() as session:
        return _rekordbox(lambda: SetPlannerService(session).list_playlists(under, depth))


@mcp.tool()
def get_rekordbox_playlist_tracks(playlist: str, recursive: bool = True) -> Dict[str, Any]:
    """rekordbox のプレイリスト（またはフォルダ配下すべて）の曲を、DJ の並び順で
    plumdeck の track id に照合して返す。playlist は ID かパス（'現場物/HIPHOP&Latin'）。
    plumdeck 未登録の曲は unmatched に入り、セットリスト候補にはならない。
    各曲の BPM/キー/ジャンル/エネルギーを見て、フェーズ分けや曲数の判断に使う。"""
    with db_session() as session:
        return _rekordbox(lambda: SetPlannerService(session).playlist_tracks(playlist, recursive))


@mcp.tool()
def plan_phased_setlist(
    phases: List[Dict[str, Any]],
    exclude_track_ids: Optional[List[int]] = None,
    exclude_artists: Optional[List[str]] = None,
    opening_track_id: Optional[int] = None,
    include_wordplay_targets: bool = True,
    wordplay_bonus: float = 0.25,
    save_as_setlist_name: Optional[str] = None,
) -> Dict[str, Any]:
    """展開（フェーズ）ごとの候補プールから、繋ぎと流れを考慮してセット全体を組む。
    フェーズは指定順に並び、各フェーズ内は BPM と音色の類似度の繋ぎやすさ（キーは採点しない）、
    エネルギー/BPM の推移目標、承認済みワードプレイで順序を決める。
    フェーズの切り替わり（最初の1曲）は前フェーズ最後の曲との繋ぎを特に重視する。

    phases の各要素（label と count は必須、候補ソースは1つ以上推奨）:
      label: 展開名（例: 'ウォームアップ', 'HIPHOP ピーク', 'Latin で切り替え'）
      count: そのフェーズの曲数 (1..60)
      rekordbox_playlists: rekordbox のプレイリスト/フォルダの ID かパスのリスト（フォルダは配下全部）
      track_ids: 候補に入れる曲 id
      anchor_track_ids: 必ず入れる曲 id（DJ が明示した曲）
      artists: 好きなアーティスト名（部分一致）。その曲を候補に入れる
      similar_to_track_ids: この曲たちに音響的に似た曲を候補に足す
      expand_similar: true ならこのフェーズの候補全曲を種に類似曲を足す（最大12曲分）
      similar_per_seed: 種1曲あたりの類似曲数 (1..30, 既定8)
      genres / subgenres / year_min / year_max: 拡張候補（artists/類似/ワードプレイ/条件検索）の絞り込み。
        プレイリスト・track_ids・anchor は DJ が選んだ曲としてそのまま使う
      energy_start / energy_end: フェーズ内のエネルギー推移目標 (0..1)
      bpm_start / bpm_end: フェーズ内の BPM 推移目標
      keep_order: true ならプレイリスト等の並び順をそのまま使う
    候補ソースが1つもないフェーズは genres/年代/エネルギー/BPM でライブラリ全体から探す。

    exclude_artists は部分一致で除外する（anchor/track_ids で明示した曲は除外しない）。
    opening_track_id は直前にかかっている曲（結果には含めず、最初の繋ぎの起点にする）。
    include_wordplay_targets=true なら候補曲から承認済みワードプレイで繋がる曲も
    その直後専用の候補として加える（pending のペアは使わない）。
    wordplay_bonus (0..1, 既定0.25) は承認済みワードプレイで繋がる遷移への加点。
    DJ がワードプレイを多めに入れたいときは 0.5〜0.8 に上げる（繋ぎやすさより優先される）。
    保存しない場合は結果の track_data を set_setlist_tracks に渡すとワードプレイ注釈ごと保存できる。
    save_as_setlist_name を渡すとその名前で新しいセットリストを作成して保存する。"""
    with db_session() as session:
        return _rekordbox(lambda: SetPlannerService(session).plan(
            phases,
            exclude_track_ids=exclude_track_ids,
            exclude_artists=exclude_artists,
            opening_track_id=opening_track_id,
            include_wordplay_targets=include_wordplay_targets,
            wordplay_bonus=wordplay_bonus,
            save_as_setlist_name=save_as_setlist_name,
        ))


@mcp.prompt(
    title="プレイリストからセットを組む",
    description="rekordbox のフォルダ/プレイリストを展開ごとに読み込んで理想的なセットリストを作る",
)
def setlist_from_rekordbox_playlists(playlists: str = "", minutes: str = "") -> str:
    return f"""rekordbox のプレイリストを素材に、展開のあるセットリストを plumdeck で組んでください。

対象プレイリスト（DJ の指定）: {playlists or "未指定 — list_rekordbox_playlists で候補を見せて選んでもらう"}
想定時間: {minutes or "未指定 — DJ に確認する"}

手順:
1. list_rekordbox_playlists で階層を確認し、指定されたフォルダ/プレイリストを特定する。
2. get_rekordbox_playlist_tracks で各プレイリストの曲・BPM・キー・ジャンル・エネルギーを確認する。
3. プレイリストをどの順で展開させるか（例: 現場物/イベ2 で温めて 現場物/HIPHOP&Latin で切り替える）、
   各展開の曲数、エネルギー/BPM の推移を DJ の意図に沿って決め、短く説明して確認を取る。
   曲数は 想定時間 ÷ 平均の使用尺（DJ に聞くか 1 曲 3〜4 分）で見積もる。
4. plan_phased_setlist に展開を phases として渡す。切り替えが急なら間に
   similar_to_track_ids や genres を使ったブリッジ用の短いフェーズを挟む。
5. 結果の transition（bpm_diff, key_relation, mix_score）と notes を見て、弱い繋ぎを
   指摘し、必要なら条件を変えて再生成する。保存は DJ の了承後に
   save_as_setlist_name か set_setlist_tracks(track_data=...) で行う。
除外したいアーティストがいれば exclude_artists に渡す。"""


@mcp.prompt(
    title="設問形式でセットを組む",
    description="流れ・好きなアーティストや曲を質問で聞き取り、類似曲やワードプレイを交えてセットを組む",
)
def setlist_questionnaire(theme: str = "") -> str:
    return f"""DJ に設問形式で好みを聞き取り、plumdeck のローカル音源だけでセットリストを組んでください。
テーマ・メモ: {theme or "なし"}

進め方（1 回に 1〜3 問、選択肢を添えて短く聞く）:
1. 現場と尺: どんな場所/時間帯か、何分のセットか、客層。
2. 流れ: 例「じわじわ上げる」「序盤から飛ばして中盤で落とす」「ジャンルを切り替えて見せ場を作る」。
   答えから 3〜5 個の展開（phase）と、それぞれのエネルギー/BPM の推移を決める。
3. 好きなアーティスト・曲: search_tracks で実在を確認し、候補を番号付きで提示して選んでもらう。
   展開ごとに誰を軸にしたいかも聞く。絶対かけたい曲は anchor_track_ids にする。
4. 広げ方: 選んだアーティストだけか、似た曲も混ぜるか（similar_to_track_ids / expand_similar）、
   年代・ジャンルの縛り、避けたいアーティスト。
5. ワードプレイ: list_wordplay_pairs で承認済みペアを確認する。選んだ曲の歌詞から
   find_wordplay_links で新しい繋ぎ候補を探し、良いものは propose_wordplay_pairs で pending 登録して
   DJ に承認を求める（承認されたものだけが生成に効く。承認は DJ の明示が必要）。
6. 聞き取った内容を phases にまとめて plan_phased_setlist を呼ぶ。各展開の label には
   意図がわかる名前を付ける。好きなアーティストは artists、選んだ曲は track_ids/anchor_track_ids。
7. 結果を「展開 / 曲 / 選ばれた理由(sources) / 繋ぎ」で提示し、気に入らない点を聞いて調整する。
   保存は DJ の了承後に save_as_setlist_name か set_setlist_tracks(track_data=...) で行う。
アーティストの類似性は音響ベクトルだけでは分からないので、似たアーティストはあなた自身の
音楽知識で挙げ、search_tracks でライブラリにあるか確かめてから artists に加える。
除外したいアーティストがいれば exclude_artists に渡す。"""
