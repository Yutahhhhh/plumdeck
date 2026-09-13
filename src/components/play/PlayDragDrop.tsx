import { DndContext, DragOverlay, MouseSensor, TouchSensor, pointerWithin, useDraggable, useDroppable, useSensor, useSensors, type DragEndEvent, type DragStartEvent } from "@dnd-kit/core";
import { useState, type ReactNode } from "react";
import type { Track } from "@/types";
import type { DeckId } from "@/types/dj-engine";
import { readJunctionLiveDrag, readPlayTrackDrag, routePlayTrackDrop, type JunctionLiveDragData, type PlayDropData, type PlayTrackDragData } from "@/services/play-drag-routing";

export function PlayDragDropProvider({ children, onLoad, onMonitorJunction }: { children: ReactNode; onLoad: (deck: DeckId, track: Track) => void; onMonitorJunction?: (deck: DeckId) => void }) {
  const sensors = useSensors(
    useSensor(MouseSensor, { activationConstraint: { distance: 5 } }),
    useSensor(TouchSensor, { activationConstraint: { delay: 150, tolerance: 5 } }),
  );
  const [active, setActive] = useState<{ title?: string | null; artist?: string | null; filepath?: string } | null>(null);
  const start = (event: DragStartEvent) => setActive(readPlayTrackDrag(event.active.data.current) ?? readJunctionLiveDrag(event.active.data.current));
  const finish = (event: DragEndEvent) => {
    setActive(null);
    routePlayTrackDrop(event.active.data.current, event.over?.data.current, onLoad, onMonitorJunction);
  };
  return <DndContext sensors={sensors} collisionDetection={pointerWithin} onDragStart={start} onDragCancel={() => setActive(null)} onDragEnd={finish}>
    {children}
    <DragOverlay dropAnimation={null}>{active ? <div className="dj-track-drag-overlay"><strong>{active.title || active.filepath}</strong><span>{active.artist || "アーティスト不明"}</span></div> : null}</DragOverlay>
  </DndContext>;
}

export function usePlayTrackDrag(id: string, track: Track) {
  return useDraggable({ id, disabled: !track.filepath, data: { kind: "play-track", track } satisfies PlayTrackDragData });
}

export function useJunctionLiveDrag(enabled: boolean) {
  return useDraggable({ id: "junction-live-source", disabled: !enabled, data: { kind: "junction-live", title: "Junction Live" } satisfies JunctionLiveDragData });
}

export function usePlayDeckDrop(id: string, deck: DeckId) {
  return useDroppable({ id, data: { kind: "deck", deck } satisfies PlayDropData });
}

export function usePlayActionDrop(id: string, accept?: (track: Track) => void) {
  return useDroppable({ id, disabled: !accept, data: accept ? { kind: "action", accept } satisfies PlayDropData : undefined });
}
