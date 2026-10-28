import { useInfiniteQuery, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { useLocation } from "wouter";

import { api, session, SIGNED_OUT_EVENT, type AuditEntry } from "../lib/api";
import { useWorkspace } from "../lib/workspace";
import { Button, Dialog, Input, relativeTime, Spinner } from "./ui";

/** Rename the workspace, or delete it with everything in it. Org admins only. */
export function GeneralSettings({ onDeleted }: { onDeleted: () => void }) {
  const ws = useWorkspace();
  const client = useQueryClient();
  const [, navigate] = useLocation();
  const [name, setName] = useState(ws.org.name);
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState<"rename" | "delete" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  async function run(kind: "rename" | "delete") {
    setBusy(kind);
    setError(null);
    setSaved(false);
    try {
      if (kind === "rename") {
        await api.renameOrg(ws.org.id, name.trim());
        setSaved(true);
      } else {
        await api.deleteOrg(ws.org.id, confirm.trim());
        onDeleted();
        navigate("/");
      }
      await client.invalidateQueries({ queryKey: ["me"] });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="space-y-6">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          void run("rename");
        }}
      >
        <label className="mb-1 block text-xs font-medium text-ink-2">Workspace name</label>
        <div className="flex gap-2">
          <Input value={name} maxLength={100} onChange={(e) => setName(e.target.value)} />
          <Button disabled={busy !== null || !name.trim() || name.trim() === ws.org.name}>
            {busy === "rename" && <Spinner />}
            Save
          </Button>
        </div>
        {saved && <p className="mt-1 text-xs text-ink-3">Saved.</p>}
      </form>

      <div className="rounded-lg border border-danger/30 p-3">
        <div className="text-sm font-medium text-danger">Delete workspace</div>
        <p className="mt-1 mb-2 text-xs text-ink-2">
          Deletes every teamspace, page, link and the audit history of {ws.org.name}, for everyone. This cannot be
          undone. Type <code className="rounded bg-chip px-1">{ws.org.slug}</code> to confirm.
        </p>
        <div className="flex gap-2">
          <Input value={confirm} onChange={(e) => setConfirm(e.target.value)} placeholder={ws.org.slug} />
          <Button
            variant="danger"
            disabled={busy !== null || confirm.trim() !== ws.org.slug}
            onClick={() => void run("delete")}
          >
            {busy === "delete" && <Spinner />}
            Delete
          </Button>
        </div>
      </div>
      {error && <p className="text-sm text-danger">{error}</p>}
    </div>
  );
}

const PAGE = 100;

function summary(entry: AuditEntry): string {
  const d = entry.details as Record<string, unknown>;
  const parts = Object.entries(d ?? {})
    .filter(([, v]) => v !== null && typeof v !== "object")
    .map(([k, v]) => `${k}: ${String(v)}`);
  return parts.join(" · ");
}

/** The organization's audit log, newest first. Org admins only. */
export function AuditLog() {
  const ws = useWorkspace();
  const [filter, setFilter] = useState("");
  const log = useInfiniteQuery({
    queryKey: ["audit", ws.org.id],
    queryFn: ({ pageParam }) => api.auditLogs(ws.org.id, PAGE, pageParam),
    initialPageParam: undefined as number | undefined,
    getNextPageParam: (last) => (last.length === PAGE ? last[last.length - 1].id : undefined),
  });
  const f = filter.trim().toLowerCase();
  const entries = (log.data?.pages.flat() ?? []).filter(
    (e) => !f || e.action.includes(f) || (e.actor_email ?? "system").toLowerCase().includes(f),
  );

  return (
    <div>
      <Input placeholder="Filter by action or person" value={filter} onChange={(e) => setFilter(e.target.value)} />
      <div className="mt-3 max-h-[420px] overflow-y-auto">
        {log.isLoading && <Spinner />}
        {log.error && <p className="text-sm text-danger">{String(log.error)}</p>}
        {entries.map((e) => (
          <div key={e.id} className="flex gap-3 border-b border-line py-1.5 text-[13px] last:border-b-0">
            <span className="w-[72px] shrink-0 text-ink-3" title={e.created_at}>
              {relativeTime(e.created_at)}
            </span>
            <div className="min-w-0 flex-1">
              <div className="flex gap-2">
                <span className="font-medium">{e.action}</span>
                <span className="truncate text-ink-2">{e.actor_email ?? "System"}</span>
              </div>
              {summary(e) && <div className="truncate text-xs text-ink-3 selectable">{summary(e)}</div>}
            </div>
          </div>
        ))}
        {log.data && entries.length === 0 && <p className="py-6 text-center text-sm text-ink-3">No entries.</p>}
      </div>
      {log.hasNextPage && (
        <Button className="mt-3" disabled={log.isFetchingNextPage} onClick={() => void log.fetchNextPage()}>
          {log.isFetchingNextPage && <Spinner />}
          Load older entries
        </Button>
      )}
    </div>
  );
}

/** Erases the user's account everywhere (GDPR). Needs a fresh sign-in. */
export function DeleteAccountDialog({ onClose }: { onClose: () => void }) {
  const ws = useWorkspace();
  const client = useQueryClient();
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function run() {
    setBusy(true);
    setError(null);
    try {
      await session.deleteAccount(ws.me.email);
      client.clear();
      window.dispatchEvent(new Event(SIGNED_OUT_EVENT));
    } catch (err) {
      setError(String(err));
      setBusy(false);
    }
  }

  return (
    <Dialog title="Delete my account" onClose={busy ? () => {} : onClose} width={460}>
      <div className="space-y-2 text-sm text-ink-2">
        <p>This permanently erases {ws.me.email} from this nanace server:</p>
        <ul className="list-disc space-y-1 pl-5">
          <li>you leave every workspace and teamspace, and your tokens and Claude connections stop working;</li>
          <li>workspaces where you are the only member are deleted with their pages;</li>
          <li>pages you wrote in shared teamspaces stay there, attributed to a deleted user.</li>
        </ul>
        <p>
          If you are the last admin of a workspace that has other members, make someone else an admin first. You will
          be asked to sign in again to confirm.
        </p>
      </div>
      <label className="mt-4 mb-1 block text-xs text-ink-2">Type your email to confirm</label>
      <Input value={confirm} onChange={(e) => setConfirm(e.target.value)} placeholder={ws.me.email} />
      {error && <p className="mt-2 text-sm text-danger selectable">{error}</p>}
      <div className="mt-4 flex items-center justify-end gap-2">
        {busy && (
          <span className="mr-auto flex items-center gap-2 text-sm text-ink-2">
            <Spinner /> Confirm your sign-in…
          </span>
        )}
        <Button onClick={onClose} disabled={busy}>
          Cancel
        </Button>
        <Button
          variant="danger"
          disabled={busy || confirm.trim().toLowerCase() !== ws.me.email.toLowerCase()}
          onClick={() => void run()}
        >
          Delete my account
        </Button>
      </div>
    </Dialog>
  );
}
