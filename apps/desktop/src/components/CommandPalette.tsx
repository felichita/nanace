import { useQuery } from "@tanstack/react-query";
import { Command } from "cmdk";
import { BrainCircuit, FileText, Network, Plug, Plus, Search, Sparkles } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { useLocation } from "wouter";

import { api } from "../lib/api";
import { useLayout } from "../lib/layout";
import { useWorkspace } from "../lib/workspace";
import { relativeTime, Spinner } from "./ui";

function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const t = window.setTimeout(() => setV(value), ms);
    return () => window.clearTimeout(t);
  }, [value, ms]);
  return v;
}

const itemClass =
  "flex h-9 cursor-default items-center gap-2.5 rounded-md px-2 text-sm data-[selected=true]:bg-hover";

export function CommandPalette({ onClose }: { onClose: () => void }) {
  const ws = useWorkspace();
  const layout = useLayout();
  const [, navigate] = useLocation();
  const [query, setQuery] = useState("");
  const q = query.trim();
  const debounced = useDebounced(q, 250);

  const semantic = useQuery({
    queryKey: ["search", ws.org.id, debounced],
    queryFn: () => api.search(ws.org.id, debounced, 8),
    enabled: debounced.length >= 3,
    staleTime: 30_000,
  });

  const titleMatches = useMemo(() => {
    const lower = q.toLowerCase();
    const all = [...ws.pages.values()];
    if (!lower) return all.sort((a, b) => b.updated_at.localeCompare(a.updated_at)).slice(0, 8);
    return all
      .filter((p) => p.title.toLowerCase().includes(lower) || p.tags.some((t) => t.includes(lower)))
      .sort((a, b) => Number(b.title.toLowerCase().startsWith(lower)) - Number(a.title.toLowerCase().startsWith(lower)))
      .slice(0, 8);
  }, [q, ws.pages]);
  const shown = new Set(titleMatches.map((p) => p.id));
  const semanticHits = (debounced === q ? semantic.data ?? [] : []).filter((h) => !shown.has(h.context_id));

  const go = (path: string) => {
    onClose();
    navigate(path);
  };

  return (
    <div className="fixed inset-0 z-50 flex items-start justify-center bg-black/40 pt-[14vh]" onMouseDown={onClose}>
      <Command
        shouldFilter={false}
        loop
        className="w-[620px] overflow-hidden rounded-xl bg-popover shadow-pop"
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={(e) => e.key === "Escape" && onClose()}
      >
        <div className="flex items-center gap-2 border-b border-line px-4">
          <Search size={18} className="text-ink-3" />
          <Command.Input
            autoFocus
            value={query}
            onValueChange={setQuery}
            placeholder={`Search ${ws.org.name}…`}
            className="h-12 flex-1 bg-transparent text-base outline-none placeholder:text-ink-3"
          />
          {semantic.isFetching && <Spinner />}
        </div>
        <Command.List className="max-h-[420px] overflow-y-auto p-1.5">
          <Command.Empty className="px-3 py-6 text-center text-sm text-ink-3">
            {q.length >= 3 && semantic.isFetching ? "Searching…" : "No results"}
          </Command.Empty>
          {titleMatches.length > 0 && (
            <Command.Group
              heading={q ? "Pages" : "Recent"}
              className="[&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:text-ink-3"
            >
              {titleMatches.map((p) => (
                <Command.Item key={p.id} value={`page-${p.id}`} onSelect={() => go(`/page/${p.id}`)} className={itemClass}>
                  <FileText size={16} className="shrink-0 text-ink-3" />
                  <span className="truncate">{p.title}</span>
                  <span className="truncate text-xs text-ink-3">{ws.team(p.team_id)?.name}</span>
                  <span className="ml-auto shrink-0 text-xs text-ink-3">{relativeTime(p.updated_at)}</span>
                </Command.Item>
              ))}
            </Command.Group>
          )}
          {semanticHits.length > 0 && (
            <Command.Group
              heading="By meaning"
              className="[&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:text-ink-3"
            >
              {semanticHits.map((h) => (
                <Command.Item
                  key={h.context_id}
                  value={`hit-${h.context_id}`}
                  onSelect={() => go(`/page/${h.context_id}`)}
                  className={`${itemClass} h-auto py-2`}
                >
                  <Sparkles size={16} className="shrink-0 self-start text-accent" />
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <span className="truncate">{h.title}</span>
                      <span className="truncate text-xs text-ink-3">{ws.team(h.team_id)?.name}</span>
                    </div>
                    <div className="line-clamp-2 text-xs text-ink-3">{h.snippet}</div>
                  </div>
                </Command.Item>
              ))}
            </Command.Group>
          )}
          <Command.Group
            heading="Actions"
            className="[&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:py-1.5 [&_[cmdk-group-heading]]:text-xs [&_[cmdk-group-heading]]:text-ink-3"
          >
            {ws.myTeams.some((t) => t.my_role === "writer" || t.my_role === "admin") && (
              <Command.Item
                value="action-new"
                onSelect={() => {
                  onClose();
                  void layout.newPage(undefined, q || undefined);
                }}
                className={itemClass}
              >
                <Plus size={16} className="text-ink-3" />
                {q ? `New page “${q}”` : "New page"}
              </Command.Item>
            )}
            {ws.myTeams.some((t) => t.my_role === "writer" || t.my_role === "admin") && (
              <Command.Item
                value="action-import-memory"
                onSelect={() => {
                  onClose();
                  layout.importMemory();
                }}
                className={itemClass}
              >
                <BrainCircuit size={16} className="text-ink-3" />
                Import Claude memory
              </Command.Item>
            )}
            <Command.Item
              value="action-connect-claude"
              onSelect={() => {
                onClose();
                layout.connectClaude();
              }}
              className={itemClass}
            >
              <Plug size={16} className="text-ink-3" />
              Connect to Claude
            </Command.Item>
            <Command.Item value="action-graph" onSelect={() => go("/graph")} className={itemClass}>
              <Network size={16} className="text-ink-3" />
              Open graph
            </Command.Item>
          </Command.Group>
        </Command.List>
      </Command>
    </div>
  );
}
