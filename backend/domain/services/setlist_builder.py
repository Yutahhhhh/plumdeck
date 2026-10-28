from typing import List, Dict, Any
import numpy as np
from domain.models.track import Track
from utils.audio_math import bpm_distance, calculate_mixability_score, SET_TRANSITION_WEIGHTS
from utils.embedding import cosine_similarity

# 同一アーティスト連続のペナルティ (単調なセットを防ぐ)
SAME_ARTIST_PENALTY = 0.2
# Human approval supplies a useful preference, not a guarantee of mixability.
APPROVED_WORDPLAY_BONUS = 0.25

class SetlistBuilder:
    """
    候補プールから、DJ的なルール（Chain Builder）に従ってセットリストを構築する責務を持つ。
    """
    
    def build_chain(
        self,
        pool: List[Dict[str, Any]],
        seeds: List[Dict[str, Any]],
        target_length: int,
        target_params: Dict[str, Any],
        approved_wordplay_edges=None,
    ) -> List[Track]:
        """Greedy Algorithm for Infinite Flow"""
        if target_length <= 0 or (not pool and not seeds):
            return []

        chain: List[Dict[str, Any]] = []
        used_ids = set()

        for s in seeds:
            if s["id"] in used_ids:
                continue
            if len(chain) >= target_length:
                break
            chain.append(s)
            used_ids.add(s["id"])

        if not chain:
            def start_score(node):
                t = node["track"]
                score = 0.0
                for feat in ("energy", "danceability", "brightness", "noisiness"):
                    if feat in target_params:
                        value = getattr(t, feat, None)
                        score -= abs(value - target_params[feat]) if value is not None else 1.0
                if "bpm" in target_params:
                    score -= bpm_distance(target_params["bpm"], t.bpm)
                return score

            starters = [node for node in pool if not node.get("wordplay_only_from_ids")]
            if not starters:
                return []
            start_node = min(starters, key=lambda node: (-start_score(node), node["id"]))
            chain.append(start_node)
            used_ids.add(start_node["id"])

        while len(chain) < target_length:
            current_node = chain[-1]
            best_next = None
            best_score = -999.0

            for candidate in pool:
                if candidate["id"] in used_ids:
                    continue
                only_from = candidate.get("wordplay_only_from_ids")
                if only_from and current_node["id"] not in only_from:
                    continue

                mix_score = self._calculate_transition_score(current_node, candidate)

                # Vibe 近接スコア: energy だけでなく danceability / brightness も評価
                vibe_score = 0.0
                for feat in ("energy", "danceability", "brightness", "noisiness"):
                    if feat in target_params:
                        cand_val = getattr(candidate["track"], feat, None) or 0.0
                        vibe_score -= abs(cand_val - target_params[feat]) * 0.1

                # 同一アーティスト連続のペナルティ
                artist_penalty = 0.0
                cur_artist = (current_node["track"].artist or "").strip().lower()
                cand_artist = (candidate["track"].artist or "").strip().lower()
                if cur_artist and cur_artist == cand_artist:
                    artist_penalty = SAME_ARTIST_PENALTY

                wordplay_bonus = APPROVED_WORDPLAY_BONUS if (
                    current_node["id"], candidate["id"]
                ) in (approved_wordplay_edges or ()) else 0.0
                total_score = mix_score + vibe_score - artist_penalty + wordplay_bonus

                if total_score > best_score or (
                    total_score == best_score and best_next is not None and candidate["id"] < best_next["id"]
                ):
                    best_score = total_score
                    best_next = candidate
            
            if best_next:
                chain.append(best_next)
                used_ids.add(best_next["id"])
            else:
                break
                
        return [node["track"] for node in chain]

    def build_path(
        self,
        pool: List[Dict[str, Any]],
        start_node: Dict[str, Any],
        end_node: Dict[str, Any],
        steps: int,
        approved_wordplay_edges=None,
    ) -> List[Track]:
        """
        Pathfinding (Bridge Mode): StartとEndの間を滑らかに埋める
        """
        chain = [start_node]
        used_ids = {start_node["id"], end_node["id"]}
        current_node = start_node
        
        # 中間ステップ数
        intermediate_steps = max(0, steps - 2)
        
        for i in range(intermediate_steps):
            progress = (i + 1) / (intermediate_steps + 1)
            
            # Linear interpolation of BPM/Energy target
            target_bpm = start_node["track"].bpm + (end_node["track"].bpm - start_node["track"].bpm) * progress
            target_energy = start_node["track"].energy + (end_node["track"].energy - start_node["track"].energy) * progress
            
            best_next = None
            best_score = -999.0
            
            for candidate in pool:
                if candidate["id"] in used_ids: continue
                only_from = candidate.get("wordplay_only_from_ids")
                if only_from and current_node["id"] not in only_from:
                    continue
                
                # 1. Mixability from Current
                mix_score = self._calculate_transition_score(current_node, candidate)
                
                # 2. Vector Similarity to End Node (Guide towards goal)
                goal_sim = cosine_similarity(candidate["vector"], end_node["vector"],
                                             candidate.get("embedding_model"), end_node.get("embedding_model"))

                # 3. Param proximity to interpolation target
                param_score = 0.0
                if candidate["track"].bpm > 0:
                    param_score -= abs(candidate["track"].bpm - target_bpm) * 0.01
                param_score -= abs(candidate["track"].energy - target_energy)
                
                # Preserve the existing scale while excluding an unavailable
                # goal embedding, just as the transition scorer does.
                total_score = (mix_score * 1.5) + (param_score * 0.5)
                if goal_sim is None:
                    total_score *= 3.0 / 2.0
                else:
                    total_score += goal_sim
                if (current_node["id"], candidate["id"]) in (approved_wordplay_edges or ()):
                    total_score += APPROVED_WORDPLAY_BONUS
                
                if total_score > best_score:
                    best_score = total_score
                    best_next = candidate
            
            if best_next:
                chain.append(best_next)
                used_ids.add(best_next["id"])
                current_node = best_next
            else:
                break
        
        chain.append(end_node)
        return [node["track"] for node in chain]

    def _calculate_transition_score(self, current: Dict[str, Any], candidate: Dict[str, Any]) -> float:
        """ラッパー: utilsの計算ロジックを呼び出す"""
        vec_sim = cosine_similarity(current["vector"], candidate["vector"],
                                    current.get("embedding_model"), candidate.get("embedding_model"))
            
        return calculate_mixability_score(
            target_bpm=current["track"].bpm,
            target_key=current["track"].key,
            candidate_bpm=candidate["track"].bpm,
            candidate_key=candidate["track"].key,
            vector_similarity=vec_sim,
            weights=SET_TRANSITION_WEIGHTS,
        )
