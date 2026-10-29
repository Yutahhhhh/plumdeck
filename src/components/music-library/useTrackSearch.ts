import { useState, useEffect, useCallback, useRef, useMemo } from "react";
import { Track } from "@/types";
import { tracksService } from "@/services/tracks";
import { FilterState } from "./types";
import { INITIAL_FILTERS } from "./constants";
import { buildTrackSearchParams } from "./utils";
import { toast } from "@/components/ui/toast";
import { getErrorDetail } from "@/services/api-client";

interface UseTrackSearchProps {
  initialFilters?: FilterState;
  limit?: number;
  extraParams?: Record<string, any>;
}

export function useTrackSearch({ initialFilters = INITIAL_FILTERS, limit = 50, extraParams = {} }: UseTrackSearchProps = {}) {
  const [query, setQuery] = useState("");
  const [tracks, setTracks] = useState<Track[]>([]);
  const [loading, setLoading] = useState(false);
  const [filters, setFilters] = useState<FilterState>(initialFilters);
  const [isFilterOpen, setIsFilterOpen] = useState(false);
  const [page, setPage] = useState(0);
  const [hasMore, setHasMore] = useState(true);
  const [totalCount, setTotalCount] = useState<number | null>(null);

  // リクエスト世代管理: 古いレスポンスが新しい結果を上書きするのを防ぐ
  const requestSeq = useRef(0);

  const extraParamsKey = useMemo(
    () => JSON.stringify(extraParams),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [JSON.stringify(extraParams)]
  );

  const search = useCallback(async (resetPage = false) => {
    const seq = ++requestSeq.current;
    setLoading(true);
    try {
      const currentPage = resetPage ? 0 : page;
      const offset = currentPage * limit;

      const params = buildTrackSearchParams(query, filters, limit, offset);
      const finalParams = { ...params, ...extraParams };
      const data = await tracksService.getTracks(finalParams);

      // 自分より新しいリクエストが発行済みなら結果を破棄
      if (seq !== requestSeq.current) return;

      if (resetPage) {
        setTracks(data);
        setPage(1);
      } else {
        setTracks(prev => [...prev, ...data]);
        setPage(prev => prev + 1);
      }

      setHasMore(data.length === limit);

      // リセット検索時は総件数も並行取得 (表示用)
      if (resetPage) {
        const { limit: _l, offset: _o, ...countParams } = finalParams;
        tracksService
          .getCount(countParams)
          .then((res) => {
            if (seq === requestSeq.current) setTotalCount(res.count);
          })
          .catch(() => {
            if (seq === requestSeq.current) setTotalCount(null);
          });
      }
    } catch (error) {
      if (seq !== requestSeq.current) return;
      console.error("Search failed", error);
      toast.error("検索に失敗しました", getErrorDetail(error));
    } finally {
      if (seq === requestSeq.current) setLoading(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query, filters, limit, page, extraParamsKey]);

  const searchRef = useRef(search);
  useEffect(() => {
    searchRef.current = search;
  }, [search]);

  // テキスト入力 (query) のみデバウンス
  const isFirstQueryRun = useRef(true);
  useEffect(() => {
    if (isFirstQueryRun.current) {
      isFirstQueryRun.current = false;
      // 初回マウント時は即時検索
      searchRef.current(true);
      return;
    }
    const t = setTimeout(() => searchRef.current(true), 300);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query]);

  // フィルタ・外部パラメータの変更は即時反映 (Apply ボタン押下時に待たせない)
  const isFirstFilterRun = useRef(true);
  useEffect(() => {
    if (isFirstFilterRun.current) {
      isFirstFilterRun.current = false;
      return;
    }
    searchRef.current(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [filters, extraParamsKey]);

  // Newly imported tracks appear without a manual reload while the list is
  // still at its first page (a deeper scroll position is left alone).
  const pageRef = useRef(page);
  pageRef.current = page;
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const refresh = () => {
      if (pageRef.current > 1) return;
      clearTimeout(timer);
      timer = setTimeout(() => searchRef.current(true), 1500);
    };
    window.addEventListener("plumdeck:import-tracks-updated", refresh);
    return () => { clearTimeout(timer); window.removeEventListener("plumdeck:import-tracks-updated", refresh); };
  }, []);

  const loadMore = () => {
    if (!loading && hasMore) {
      search(false);
    }
  };

  const applyFilters = (newFilters: FilterState) => {
    setFilters(newFilters);
    setIsFilterOpen(false);
  };

  const clearAllFilters = () => {
    setQuery("");
    setFilters(INITIAL_FILTERS);
  };

  const activeFilterCount = Object.keys(filters).filter((k) => {
    const key = k as keyof FilterState;
    if (key === "bpmRange") return false;
    if (key === "minEnergy" || key === "maxEnergy")
      return filters.minEnergy > 0 || filters.maxEnergy < 1;
    if (key === "minDanceability" || key === "maxDanceability")
      return filters.minDanceability > 0 || filters.maxDanceability < 1;
    if (key === "minBrightness" || key === "maxBrightness")
      return filters.minBrightness > 0 || filters.maxBrightness < 1;

    const val = filters[key];
    if (Array.isArray(val)) return val.length > 0;
    return val !== null && val !== "" && val !== 0;
  }).length;

  return {
    query,
    setQuery,
    tracks,
    loading,
    filters,
    setFilters,
    isFilterOpen,
    setIsFilterOpen,
    applyFilters,
    clearAllFilters,
    search,
    loadMore,
    hasMore,
    activeFilterCount,
    setTracks,
    totalCount,
  };
}
