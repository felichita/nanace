import { useQuery, useQueryClient } from "@tanstack/react-query";
import { FileText, RotateCcw, Search, Trash2 } from "lucide-react";
import { useState } from "react";
import { useLocation } from "wouter";

import { api, type TrashItem } from "../lib/api";
import { useWorkspace } from "../lib/workspace";
import { Button, Dialog, IconButton, relativeTime, Spinner } from "./ui";

/** Trash: restore or delete forever. Only teamspaces the user can edit are listed. */
export function TrashDialog({ onClose }: { onClose: () => void }) {
  const ws = useWorkspace();
  const client = useQueryClient();
  const [, navigate] = useLocation();
  const orgId = ws.org.id;
  const trash = useQuery({ queryKey: ["trash", orgId], queryFn: () => api.trash(orgId) });
  const [filter, setFilter] = useState("");
  const [confirm, setConfirm] = useState<TrashItem | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const items = (trash.data ?? []).filter((t) => t.title.toLowerCase().includes(filter.trim().toLowerCase()));
  const sample = trash.data?.find((t) => t.purge_at);
  const retentionDays = sample?.purge_at
    ? Math.round((Date.parse(sample.purge_at) - Date.parse(sample.deleted_at)) / 86_400_000)
    : null;

  async function act(item: TrashItem, kind: "restore" | "purge") {
    setBusy(item.id);
    setError(null);
    try {
      if (kind === "restore") {
        await api.restore(orgId, item.id);
        ws.refreshPages(item.team_id);
      } else {
        await api.purge(orgId, item.id);
      }
      await client.invalidateQueries({ queryKey: ["trash", orgId] });
      void client.invalidateQueries({ queryKey: ["links", orgId] });
      if (kind === "restore") {
        onClose();
        navigate(`/page/${item.id}`);
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
      setConfirm(null);
    }
  }

  return (
    <Dialog title="Trash" onClose={onClose} width={560}>
      <div className="mb-3 flex items-center gap-2 rounded-md bg-hover px-2">
        <Search size={14} className="text-ink-3" />
        <input
          autoFocus
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Search pages in Trash"
          className="h-8 flex-1 bg-transparent text-sm outline-none placeholder:text-ink-3"
        />
      </div>
      {error && <p className="mb-2 text-sm text-danger">{error}</p>}
      <div className="max-h-[360px] min-h-[120px] overflow-y-auto">
        {trash.isLoading && (
          <div className="flex justify-center p-6">
            <Spinner />
          </div>
        )}
        {trash.data && items.length === 0 && (
          <div className="flex flex-col items-center gap-2 p-8 text-sm text-ink-3">
            <Trash2 size={28} strokeWidth={1.5} />
            {filter ? "No matches in Trash" : "Trash is empty"}
          </div>
        )}
        {items.map((item) => {
          const team = ws.team(item.team_id);
          return (
            <div key={item.id} className="group flex h-11 items-center gap-2 rounded-md px-2 hover:bg-hover">
              <FileText size={16} className="shrink-0 text-ink-3" />
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm">{item.title}</div>
                <div className="truncate text-xs text-ink-3">
                  {team?.name} · deleted {relativeTime(item.deleted_at)} by {item.deleted_by_email}
                  {item.purge_at && ` · gone for good ${untilText(item.purge_at)}`}
                </div>
              </div>
              {busy === item.id ? (
                <Spinner />
              ) : (
                <>
                  <IconButton title="Restore" className="h-7 w-7" onClick={() => void act(item, "restore")}>
                    <RotateCcw size={15} />
                  </IconButton>
                  <IconButton title="Delete forever" className="h-7 w-7" onClick={() => setConfirm(item)}>
                    <Trash2 size={15} />
                  </IconButton>
                </>
              )}
            </div>
          );
        })}
      </div>
      <p className="mt-3 text-xs text-ink-3">
        Pages in Trash are hidden from search, the graph and Claude. Restoring brings back the page with its links.
        {retentionDays !== null && ` Pages are deleted for good ${retentionDays} days after they are moved to Trash.`}
      </p>

      {confirm && (
        <Dialog title="Delete forever?" onClose={() => setConfirm(null)} width={400}>
          <p className="mb-4 text-sm text-ink-2">
            “{confirm.title}” will be permanently deleted. This cannot be undone.
          </p>
          <div className="flex justify-end gap-2">
            <Button onClick={() => setConfirm(null)}>Cancel</Button>
            <Button variant="danger" onClick={() => void act(confirm, "purge")}>
              Delete forever
            </Button>
          </div>
        </Dialog>
      )}
    </Dialog>
  );
}

function untilText(iso: string): string {
  const hours = Math.max(0, (Date.parse(iso) - Date.now()) / 3_600_000);
  if (hours < 1) return "within the hour";
  if (hours < 24) return `in ${Math.round(hours)}h`;
  return `in ${Math.round(hours / 24)} days`;
}

export function Toast({
  message,
  action,
  onClose,
}: {
  message: string;
  action?: { label: string; run: () => void };
  onClose: () => void;
}) {
  return (
    <div className="fixed bottom-5 left-1/2 z-[60] flex -translate-x-1/2 items-center gap-3 rounded-lg bg-[#2f2f2f] px-4 py-2.5 text-sm text-white shadow-pop">
      <span>{message}</span>
      {action && (
        <button
          className="font-medium text-[#7fb6ff] hover:underline"
          onClick={() => {
            action.run();
            onClose();
          }}
        >
          {action.label}
        </button>
      )}
    </div>
  );
}
