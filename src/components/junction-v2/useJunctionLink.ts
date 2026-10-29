import { useSyncExternalStore } from "react";
import { linkStore, type LinkSnapshot } from "@/services/junction-v2/session";
import { nextPerformer, participant, roleOf, type Role } from "@/services/junction-v2/core/turns";

export function useJunctionLink(): LinkSnapshot {
  return useSyncExternalStore(linkStore.subscribe, linkStore.get);
}

export const ROLE_LABEL: Record<Role, string> = { onAir: "ON AIR", next: "NEXT", previous: "前のDJ", idle: "" };

export function nameOf(s: LinkSnapshot, id: string | null): string {
  if (!id) return "—";
  return participant(s.session!, id)?.name ?? "DJ";
}

/** Derived view used by the panel and the Play strip. */
export function turnView(s: LinkSnapshot) {
  const session = s.session;
  if (!session || !s.me) return null;
  const next = nextPerformer(session);
  const myRole = roleOf(session, s.me);
  const isHost = s.role === "host";
  return {
    session,
    onAir: session.onAir,
    next,
    previous: session.previous,
    myRole,
    canNext: Boolean(next) && (isHost || myRole === "onAir" || myRole === "next"),
    canRelease: Boolean(session.previous) && (isHost || myRole === "onAir"),
  };
}
