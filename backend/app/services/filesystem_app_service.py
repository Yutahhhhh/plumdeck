import os
import re
import urllib.parse
import base64
from typing import List, Set, Dict, Any, Optional
from sqlmodel import Session, select
from domain.constants import SUPPORTED_EXTENSIONS
from utils.filesystem import resolve_path
from utils.metadata import extract_full_metadata, update_file_metadata
from utils.ingestion import completed_analysis_condition, has_completed_analysis, has_valid_embedding
from domain.services.analysis.constants import EMBEDDING_MODEL
from domain.models.track import Track, TrackAnalysis, TrackEmbedding

def _is_supported_file(filename: str) -> bool:
    return filename.lower().endswith(SUPPORTED_EXTENSIONS)

class FilesystemAppService:
    def __init__(self, session: Session):
        self.session = session

    def get_track_metadata(self, track_id: int) -> Dict[str, Any]:
        track = self.session.get(Track, track_id)
        if not track:
            return None

        resolved_path = resolve_path(track.filepath)
        if not resolved_path:
            return None
        
        # Beat positions are served by the grid endpoints; no client read them
        # here, and they cost ~8KB per Play list row.
        analysis = self.session.get(TrackAnalysis, track_id)
        db_metadata = {
            "bpm": track.bpm,
            "key": track.key,
            "waveform_peaks": analysis.waveform_peaks if analysis else [],
            "analyzed": has_completed_analysis(track, analysis=analysis),
        }

        tag_metadata = extract_full_metadata(resolved_path)

        return {
            **tag_metadata,
            **db_metadata
        }

    def list_directory(self, path: str) -> List[Dict[str, Any]]:
        search_path = resolve_path(path)
        if not search_path:
            return None
        
        try:
            items = []

            with os.scandir(search_path) as it:
                for entry in it:
                    if entry.name.startswith('.'):
                        continue
                    
                    if entry.is_dir():
                        pass
                    
                    else:
                        if not _is_supported_file(entry.name):
                            continue

                    item = {
                        "name": entry.name,
                        "path": entry.path,
                        "is_dir": entry.is_dir(),
                        "is_analyzed": False
                    }
                    items.append(item)

            items.sort(key=lambda x: (not x['is_dir'], x['name'].lower()))
            
            file_paths = [item['path'] for item in items if not item['is_dir']]
            
            analyzed_files: Dict[str, str] = {}
            embedded_files: Set[str] = set()
            if file_paths:
                # A Track row can be created before expensive audio analysis starts
                # (for example by Play mode's fast import).  Its mere presence does
                # not mean analysis succeeded.  The shared analyzed rule keeps
                # partially imported and failed tracks visible for a retry.
                statement = (
                    select(Track.filepath, Track.analysis_level, TrackEmbedding.model_name,
                           TrackEmbedding.embedding_json)
                    .outerjoin(TrackEmbedding, Track.id == TrackEmbedding.track_id)
                    .where(Track.filepath.in_(file_paths), completed_analysis_condition())
                )
                for filepath, level, model, vector in self.session.exec(statement).all():
                    analyzed_files[filepath] = level or "full"
                    # The background job's rule: a vector of another model or an
                    # invalid one is still pending.
                    if model == EMBEDDING_MODEL and has_valid_embedding(vector):
                        embedded_files.add(filepath)

            final_result = []
            for item in items:
                if not item['is_dir']:
                    item['is_analyzed'] = item['path'] in analyzed_files
                    item['analysis_level'] = analyzed_files.get(item['path'])
                    # Similarity vectors are optional and filled in the background.
                    item['embedding_ready'] = item['path'] in embedded_files
                final_result.append(item)
                
            return final_result

        except Exception as e:
            import traceback
            traceback.print_exc()
            raise e

    def update_metadata_content(self, track_id: int, lyrics: Optional[str] = None, artwork_data: Optional[str] = None) -> bool:
        track = self.session.get(Track, track_id)
        if not track or not track.filepath:
            raise ValueError("Track not found")
        
        if lyrics is not None or artwork_data is not None:
            success = update_file_metadata(track.filepath, lyrics=lyrics, artwork_b64=artwork_data)
            if not success:
                raise RuntimeError("Failed to write metadata to file")
        
        return True

    def _clean_title_for_search(self, title: str) -> str:
        return re.sub(r'\s*[\(\[].*?[\)\]]', '', title).strip()

    def fetch_artwork_info(self, track_id: int) -> str:
        import requests

        track = self.session.get(Track, track_id)
        if not track:
            raise ValueError("Track not found")
        
        query = f"{track.artist} {track.album}" if track.album else f"{track.artist} {track.title}"
        
        try:
            encoded_query = urllib.parse.quote(query)
            url = f"https://itunes.apple.com/search?term={encoded_query}&media=music&entity=album&limit=1"
            
            response = requests.get(url, timeout=10)
            data = response.json()
            
            if data["resultCount"] > 0:
                result = data["results"][0]
                artwork_url = result.get("artworkUrl100")
                if artwork_url:
                    high_res_url = artwork_url.replace("100x100bb", "1000x1000bb")
                    
                    img_res = requests.get(high_res_url, timeout=10)
                    if img_res.status_code == 200:
                        b64_data = base64.b64encode(img_res.content).decode('utf-8')
                        return f"data:image/jpeg;base64,{b64_data}"
            
            return ""
            
        except Exception as e:
            print(f"Artwork fetch failed: {e}")
            return ""
