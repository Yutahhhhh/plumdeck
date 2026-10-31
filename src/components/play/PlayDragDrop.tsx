import { DndContext, DragOverlay, MouseSensor, TouchSensor, pointerWithin, useDraggable, useDroppable, useSensor, useSensors, type DragEndEvent, type DragStartEvent, type Modifier } from "@dnd-kit/core";
import { getEventCoordinates } from "@dnd-kit/utilities";
import { useEffect, useState, type ReactNode } from "react";
import type { Track } from "@/types";
import type { DeckId } from "@/types/dj-engine";
import { readPlayTrackDrag, routePlayTrackDrop, type PlayDropData, type PlayTrackDragData } from "@/services/play-drag-routing";

/** The overlay starts at the row's top-left; move it so it trails the cursor instead. */
const followCursor: Modifier = ({ activatorEvent, draggingNodeRect, transform }) => {
  const at = activatorEvent && getEventCoordinates(activatorEvent);
  if (!at || !draggingNodeRect) return transform;
  return { ...transform, x: transform.x + at.x - draggingNodeRect.left + 12, y: transform.y + at.y - draggingNodeRect.top + 12 };
};

export function PlayDragDropProvider({ children, onLoad }: { children: ReactNode; onLoad: (deck: DeckId, track: Track) => void }) {
  const sensors = useSensors(
    useSensor(MouseSensor, { activationConstraint: { distance: 5 } }),
    useSensor(TouchSensor, { activationConstraint: { delay: 150, tolerance: 5 } }),
  );
  // Belt and braces for the CSS rule: never let WebKit start its own drag
  // from inside a dnd-kit row, or the drop never completes.
  useEffect(() => {
    const block = (event: DragEvent) => {
      const node = event.target instanceof Element ? event.target : (event.target as Node | null)?.parentElement;
      if (node?.closest('[aria-roledescription="draggable"]')) event.preventDefault();
    };
    document.addEventListener("dragstart", block, true);
    return () => document.removeEventListener("dragstart", block, true);
  }, []);
  const [active, setActive] = useState<{ title?: string | null; artist?: string | null; filepath?: string } | null>(null);
  // The first drag after the window gains focus can lose its mouseup in
  // WebKit, leaving the card stuck to the cursor until the next click. When
  // the button is seen released while a drag is live, finish it right there.
  useEffect(() => {
    if (!active) return;
    const release = (event: MouseEvent) => {
      if ((event.buttons & 1) !== 0) return;
      document.dispatchEvent(new MouseEvent("mouseup", { bubbles: true, cancelable: true, button: 0, buttons: 0, clientX: event.clientX, clientY: event.clientY }));
    };
    document.addEventListener("mousemove", release, true);
    return () => document.removeEventListener("mousemove", release, true);
  }, [active]);
  const start = (event: DragStartEvent) => setActive(readPlayTrackDrag(event.active.data.current));
  const finish = (event: DragEndEvent) => {
    setActive(null);
    routePlayTrackDrop(event.active.data.current, event.over?.data.current, onLoad);
  };
  return <DndContext sensors={sensors} collisionDetection={pointerWithin} onDragStart={start} onDragCancel={() => setActive(null)} onDragEnd={finish}>
    {children}
    <DragOverlay dropAnimation={null} modifiers={[followCursor]}>{active ? <div className="dj-track-drag-overlay"><strong>{active.title || active.filepath}</strong><span>{active.artist || "アーティスト不明"}</span></div> : null}</DragOverlay>
  </DndContext>;
}

export function usePlayTrackDrag(id: string, track: Track) {
  return useDraggable({ id, disabled: !track.filepath, data: { kind: "play-track", track } satisfies PlayTrackDragData });
}

export function usePlayDeckDrop(id: string, deck: DeckId) {
  return useDroppable({ id, data: { kind: "deck", deck } satisfies PlayDropData });
}

export function usePlayActionDrop(id: string, accept?: (track: Track) => void) {
  return useDroppable({ id, disabled: !accept, data: accept ? { kind: "action", accept } satisfies PlayDropData : undefined });
}
