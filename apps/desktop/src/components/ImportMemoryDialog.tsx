import { useQuery } from "@tanstack/react-query";
import { Check, ChevronDown, ChevronRight, FileText } from "lucide-react";
import { useMemo, useState } from "react";

import { api, ApiError, canWrite } from "../lib/api";
import { buildNotes, MEMORY_TAG, scanMemory, sourceOf, type MemoryNote } from "../lib/memory";
import { useWorkspace } from "../lib/workspace";
import { Button, cx, Dialog, Spinner } from "./ui";

interface Result {
  created: number;
  updated: number;
  unchanged: number;
  inTrash: number;
  failed: { title: string; error: string }[];
}

export function ImportMemoryDialog({ onClose }: { onClose: () => void }) {
  const ws = useWorkspace();
  const writable = ws.myTeams.filter((t) => canWrite(t.my_role));
  const scan = useQuery({ queryKey: ["memory-scan"], queryFn: scanMemory, staleTime: 0 });
  const [teamId, setTeamId] = useState(writable[0]?.id ?? "");
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [expanded, setExpanded] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [update, setUpdate] = useState(false);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [result, setResult] = useState<Result | null>(null);

  const projects = useMemo(() => {
    const f = filter.trim().toLowerCase();
    return (scan.data ?? []).filter((p) => !f || p.label.toLowerCase().includes(f));
  }, [scan.data, filter]);
  const notesByProject = useMemo(
    () => new Map((scan.data ?? []).map((p) => [p.id, buildNotes([p])])),
    [scan.data],
  );
  const chosen = (scan.data ?? []).filter((p) => selected.has(p.id));
  const count = chosen.reduce((n, p) => n + p.files.length, 0);

  const toggle = (id: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  async function run() {
    const orgId = ws.org.id;
    // Built together so `[[links]]` resolve across the chosen projects.
    const notes: MemoryNote[] = buildNotes(chosen);
    const res: Result = { created: 0, updated: 0, unchanged: 0, inTrash: 0, failed: [] };
    setResult(null);
    setProgress({ done: 0, total: notes.length });

    // Pages from earlier imports are recognised by their Source footer.
    const titles = new Set(notes.map((n) => n.title.toLowerCase()));
    const candidates = ws
      .pagesOf(teamId)
      .filter((p) => p.tags.includes(MEMORY_TAG) || titles.has(p.title.toLowerCase()));
    const bySource = new Map<string, { id: string; content: string; title: string; tags: string[]; updatedAt: string }>();
    const takenTitles = new Set(ws.pagesOf(teamId).map((p) => p.title.toLowerCase()));
    for (const p of candidates) {
      try {
        const full = await api.context(orgId, p.id);
        const source = sourceOf(full.content);
        if (source) {
          bySource.set(source, {
            id: full.id,
            content: full.content,
            title: full.title,
            tags: full.tags,
            updatedAt: full.updated_at,
          });
        }
      } catch {
        // Unreadable or deleted meanwhile; treat as not imported.
      }
    }

    // Memories the user moved to the Trash are not brought back.
    const trashed = new Set<string>();
    try {
      for (const t of await api.trash(orgId)) {
        if (t.team_id === teamId && t.tags.includes(MEMORY_TAG)) trashed.add(t.title.toLowerCase());
      }
    } catch {
      // Without the trash list, nothing is skipped for it.
    }

    let done = 0;
    for (const note of notes) {
      try {
        const existing = bySource.get(note.source);
        if (existing) {
          const same =
            existing.content === note.content &&
            existing.title === note.title &&
            existing.tags.join() === [...note.tags].sort().join();
          if (same || !update) {
            res.unchanged += 1;
          } else {
            await api.updateContext(orgId, existing.id, {
              title: note.title,
              content: note.content,
              tags: note.tags,
              base_updated_at: existing.updatedAt,
            });
            res.updated += 1;
          }
        } else if (trashed.has(note.title.toLowerCase())) {
          res.inTrash += 1;
        } else {
          let title = note.title;
          if (takenTitles.has(title.toLowerCase())) title = `${title} (${note.source.split("/")[0]})`.slice(0, 200);
          await api.createContext(orgId, teamId, title, note.content, note.tags);
          takenTitles.add(title.toLowerCase());
          res.created += 1;
        }
      } catch (err) {
        res.failed.push({
          title: note.title,
          error: err instanceof ApiError || err instanceof Error ? err.message : String(err),
        });
      }
      done += 1;
      setProgress({ done, total: notes.length });
    }
    ws.refreshPages(teamId);
    setProgress(null);
    setResult(res);
    setSelected(new Set());
  }

  const busy = progress !== null;

  return (
    <Dialog title="Import Claude memory" onClose={busy ? () => {} : onClose} width={620}>
      <p className="mb-4 text-sm text-ink-2">
        Notes Claude Code saved in <code className="rounded bg-chip px-1">~/.claude/projects/*/memory</code>. Each
        memory becomes a page; <code className="rounded bg-chip px-1">[[links]]</code> between them are kept. Importing
        again skips notes that are already here.
      </p>

      {writable.length === 0 ? (
        <p className="text-sm text-danger">You need edit access to a teamspace to import.</p>
      ) : (
        <>
          <div className="mb-3 flex items-center gap-2">
            <span className="text-sm text-ink-2">Import into</span>
            <select
              value={teamId}
              onChange={(e) => setTeamId(e.target.value)}
              disabled={busy}
              className="h-8 rounded-md bg-hover px-2 text-sm outline-none"
            >
              {writable.map((t) => (
                <option key={t.id} value={t.id}>
                  {t.name}
                </option>
              ))}
            </select>
            <input
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              placeholder="Filter projects"
              className="ml-auto h-8 w-44 rounded-md bg-hover px-2 text-sm outline-none placeholder:text-ink-3"
            />
          </div>

          <div className="max-h-[320px] overflow-y-auto rounded-lg border border-line">
            {scan.isLoading && (
              <div className="flex justify-center p-6">
                <Spinner />
              </div>
            )}
            {scan.error && <p className="p-4 text-sm text-danger">{String(scan.error)}</p>}
            {scan.data && projects.length === 0 && (
              <p className="p-4 text-sm text-ink-3">No Claude memory found{filter ? " for this filter" : ""}.</p>
            )}
            {projects.map((p) => {
              const on = selected.has(p.id);
              const open = expanded === p.id;
              return (
                <div key={p.id} className="border-b border-line last:border-b-0">
                  <div className="flex h-10 items-center gap-2 px-2 hover:bg-hover">
                    <button
                      onClick={() => setExpanded(open ? null : p.id)}
                      className="flex h-6 w-6 items-center justify-center rounded text-ink-3 hover:bg-active"
                    >
                      {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                    </button>
                    <label className="flex flex-1 items-center gap-2.5 text-sm">
                      <span
                        className={cx(
                          "flex h-4 w-4 items-center justify-center rounded border",
                          on ? "border-accent bg-accent text-white" : "border-ink-3",
                        )}
                      >
                        {on && <Check size={12} strokeWidth={3} />}
                      </span>
                      <input type="checkbox" className="hidden" checked={on} disabled={busy} onChange={() => toggle(p.id)} />
                      <span className="font-medium">{p.label}</span>
                      <span className="text-ink-3">
                        {p.files.length} {p.files.length === 1 ? "note" : "notes"}
                      </span>
                    </label>
                  </div>
                  {open && (
                    <div className="pb-2 pl-[42px]">
                      {notesByProject.get(p.id)?.map((n) => (
                        <div key={n.source} className="flex h-6 items-center gap-2 text-[13px] text-ink-2">
                          <FileText size={13} className="shrink-0 text-ink-3" />
                          <span className="truncate">{n.title}</span>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              );
            })}
          </div>

          <label className="mt-3 flex items-center gap-2 text-sm text-ink-2">
            <input type="checkbox" checked={update} disabled={busy} onChange={(e) => setUpdate(e.target.checked)} />
            Update pages whose memory changed since the last import (overwrites edits made here)
          </label>

          {result && (
            <div className="mt-3 rounded-md bg-hover px-3 py-2 text-sm">
              Imported {result.created} new, updated {result.updated}, {result.unchanged} already up to date
              {result.inTrash > 0 && `, ${result.inTrash} skipped because they are in Trash`}.
              {result.failed.length > 0 && (
                <div className="mt-1 text-danger">
                  {result.failed.length} failed:{" "}
                  {result.failed.map((f) => `${f.title} (${f.error})`).join("; ")}
                </div>
              )}
            </div>
          )}

          <div className="mt-4 flex items-center justify-end gap-2">
            {progress && (
              <span className="mr-auto flex items-center gap-2 text-sm text-ink-2">
                <Spinner /> {progress.done} / {progress.total}
              </span>
            )}
            <Button onClick={onClose} disabled={busy}>
              {result ? "Done" : "Cancel"}
            </Button>
            <Button variant="primary" disabled={busy || count === 0 || !teamId} onClick={() => void run()}>
              Import {count || ""} {count === 1 ? "note" : "notes"}
            </Button>
          </div>
        </>
      )}
    </Dialog>
  );
}
