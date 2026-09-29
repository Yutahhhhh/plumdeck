"""Read-only access to the rekordbox playlist tree.

The DJ's own folder structure (for example ``現場物/イベ2``) already encodes
which tracks belong to which part of a night. Set planning reads that tree
through the same read-only ``query_only`` connection as the collection lookup,
so the running rekordbox is never disturbed.
"""
from __future__ import annotations

from dataclasses import dataclass, field
import unicodedata

from infra import rekordbox_library

# djmdPlaylist.Attribute values.
KIND_PLAYLIST = 0
KIND_FOLDER = 1
KIND_SMART = 4
_KIND_NAMES = {KIND_PLAYLIST: "playlist", KIND_FOLDER: "folder", KIND_SMART: "smart"}
ROOT_ID = "root"
PATH_SEPARATOR = "/"


@dataclass
class PlaylistNode:
    id: str
    name: str
    parent_id: str
    kind: str
    seq: int
    track_count: int = 0
    path: str = ""
    children: list["PlaylistNode"] = field(default_factory=list)


@dataclass(frozen=True)
class PlaylistEntry:
    """One track slot of a playlist, in the DJ's own order."""

    playlist_id: str
    playlist_path: str
    track_no: int
    content_id: str
    filepath: str
    title: str
    artist: str


def clean_name(name: str | None) -> str:
    """Drop control characters that rekordbox occasionally stores in names."""
    text = "".join(ch for ch in (name or "") if unicodedata.category(ch)[0] != "C")
    return text.strip()


def _name_key(name: str) -> str:
    return rekordbox_library.normalize_text(clean_name(name))


def load_tree() -> list[PlaylistNode]:
    """Return the root-level nodes, each carrying its full ``path``."""
    connection = rekordbox_library._connect(rekordbox_library._database())
    try:
        rows = connection.execute(
            "SELECT p.ID, p.Name, p.ParentID, p.Attribute, p.Seq, "
            "(SELECT count(*) FROM djmdSongPlaylist s "
            " WHERE s.PlaylistID = p.ID AND s.rb_local_deleted = 0) "
            "FROM djmdPlaylist p WHERE p.rb_local_deleted = 0"
        ).fetchall()
    except Exception as error:
        raise rekordbox_library.RekordboxLibraryUnavailable(
            "rekordbox のプレイリストを読み取れませんでした"
        ) from error
    finally:
        connection.close()

    nodes = {
        str(row[0]): PlaylistNode(
            id=str(row[0]), name=clean_name(row[1]), parent_id=str(row[2] or ROOT_ID),
            kind=_KIND_NAMES.get(row[3], "unknown"), seq=int(row[4] or 0),
            track_count=int(row[5] or 0),
        )
        for row in rows
    }
    roots: list[PlaylistNode] = []
    for node in nodes.values():
        parent = nodes.get(node.parent_id)
        (parent.children if parent else roots).append(node)

    def finish(children: list[PlaylistNode], prefix: str) -> None:
        children.sort(key=lambda node: (node.seq, node.name, node.id))
        for node in children:
            node.path = f"{prefix}{PATH_SEPARATOR}{node.name}" if prefix else node.name
            finish(node.children, node.path)

    finish(roots, "")
    return roots


def iter_nodes(roots: list[PlaylistNode]):
    for node in roots:
        yield node
        yield from iter_nodes(node.children)


def find_node(roots: list[PlaylistNode], ref: str) -> PlaylistNode:
    """Resolve a playlist by its rekordbox ID or by a ``Folder/Sub/Name`` path.

    Paths compare per segment with case/width folding. A path that matches
    several nodes (rekordbox allows duplicate names) is rejected rather than
    guessed, so the caller can retry with the ID.
    """
    ref = (ref or "").strip()
    if not ref:
        raise ValueError("playlist の ID またはパスを指定してください")
    by_id = [node for node in iter_nodes(roots) if node.id == ref]
    if by_id:
        return by_id[0]
    wanted = [_name_key(part) for part in ref.split(PATH_SEPARATOR) if part.strip()]
    matches = [
        node for node in iter_nodes(roots)
        if [_name_key(part) for part in node.path.split(PATH_SEPARATOR)] == wanted
    ]
    if not matches:
        raise ValueError(f"rekordbox にプレイリスト「{ref}」が見つかりません")
    if len(matches) > 1:
        ids = ", ".join(node.id for node in matches)
        raise ValueError(f"「{ref}」に一致するプレイリストが複数あります。ID で指定してください: {ids}")
    return matches[0]


def playlist_entries(node: PlaylistNode, recursive: bool = True) -> tuple[list[PlaylistEntry], list[str]]:
    """Tracks of a playlist, or of every playlist below a folder, in DJ order.

    Returns (entries, notes). Smart playlists hold a rule, not a track list, and
    are reported in notes instead of being evaluated here.
    """
    notes: list[str] = []
    playlists: list[PlaylistNode] = []

    def collect(current: PlaylistNode) -> None:
        if current.kind == "playlist":
            playlists.append(current)
        elif current.kind == "smart":
            notes.append(f"「{current.path}」はインテリジェントプレイリストのため読み込めません")
        elif current.kind == "folder" and (recursive or current is node):
            for child in current.children:
                if child.kind == "folder" and not recursive:
                    continue
                collect(child)

    collect(node)
    if not playlists:
        return [], notes

    connection = rekordbox_library._connect(rekordbox_library._database())
    entries: list[PlaylistEntry] = []
    try:
        for playlist in playlists:
            rows = connection.execute(
                "SELECT s.TrackNo, c.ID, c.FolderPath, c.Title, a.Name "
                "FROM djmdSongPlaylist s "
                "JOIN djmdContent c ON c.ID = s.ContentID "
                "LEFT JOIN djmdArtist a ON a.ID = c.ArtistID "
                "WHERE s.PlaylistID = ? AND s.rb_local_deleted = 0 AND c.rb_local_deleted = 0 "
                "ORDER BY s.TrackNo, s.ID",
                (playlist.id,),
            ).fetchall()
            entries += [
                PlaylistEntry(
                    playlist_id=playlist.id, playlist_path=playlist.path,
                    track_no=int(track_no or 0), content_id=str(content_id),
                    filepath=str(folder_path or ""), title=str(title or ""),
                    artist=str(artist or ""),
                )
                for track_no, content_id, folder_path, title, artist in rows
            ]
    except Exception as error:
        raise rekordbox_library.RekordboxLibraryUnavailable(
            "rekordbox のプレイリストを読み取れませんでした"
        ) from error
    finally:
        connection.close()
    return entries, notes
