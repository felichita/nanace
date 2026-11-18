import type { TeamRole } from "./api";

/** Shown wherever a `Policy:` or `Wiki:` page cannot be edited. */
export const PROTECTED_NOTE = "Edited only by the teamspace owner";

/**
 * True for pages only a team admin may write: titles starting with `Wiki:` or
 * `Policy:`. Mirrors the server's rule (NFKC, invisible format characters
 * ignored, ASCII case only) so the app can show the page read-only up front;
 * the server decides.
 */
export function isProtectedTitle(title: string): boolean {
  return /^\s*([wW][iI][kK][iI]|[pP][oO][lL][iI][cC][yY])\s*:/.test(title.normalize("NFKC").replace(/\p{Cf}/gu, ""));
}

/** Whether someone with `role` in a team may edit its page titled `title`. */
export function canEditPage(role: TeamRole | null | undefined, title: string | undefined): boolean {
  if (role === "admin") return true;
  return role === "writer" && !(title !== undefined && isProtectedTitle(title));
}
