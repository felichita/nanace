import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Check, Monitor, PenLine, ShieldCheck, Terminal } from "lucide-react";
import { useState } from "react";

import { claude, type ClaudeTarget, type ClaudeTargetStatus } from "../lib/api";
import { useWorkspace } from "../lib/workspace";
import { Button, cx, Dialog, relativeTime, Spinner } from "./ui";

const TARGETS: { id: ClaudeTarget; name: string; icon: React.ReactNode; missing: string; after: string }[] = [
  {
    id: "code",
    name: "Claude Code",
    icon: <Terminal size={18} />,
    missing: "The `claude` CLI was not found. Install Claude Code first.",
    after: "Start a new Claude Code session to use it.",
  },
  {
    id: "desktop",
    name: "Claude Desktop",
    icon: <Monitor size={18} />,
    missing: "Claude Desktop is not installed; the setting is saved for when you install it.",
    after: "Restart Claude Desktop to use it.",
  },
];

function StatusChip({ status, serverUrl }: { status: ClaudeTargetStatus; serverUrl: string }) {
  if (!status.connected) return <span className="text-xs text-ink-3">Not connected</span>;
  const elsewhere = status.api_url && status.api_url !== serverUrl;
  return (
    <span className={cx("flex items-center gap-1 text-xs", elsewhere ? "text-ink-2" : "text-[#0f7b6c]")}>
      <Check size={12} strokeWidth={3} />
      {elsewhere ? `Connected to ${status.api_url}` : "Connected"}
      <span className="text-ink-3">· {status.writes ? "can edit" : "read-only"}</span>
      {status.rotated_at && (
        <span className="text-ink-3">
          · token renewed {relativeTime(new Date(status.rotated_at * 1000).toISOString())}
        </span>
      )}
    </span>
  );
}

export function ConnectClaudeDialog({ serverUrl, onClose }: { serverUrl: string; onClose: () => void }) {
  const ws = useWorkspace();
  const client = useQueryClient();
  const status = useQuery({ queryKey: ["claude-status"], queryFn: claude.status, staleTime: 0 });
  const [orgId, setOrgId] = useState(ws.org.id);
  const [chosen, setChosen] = useState<Set<ClaudeTarget> | null>(null);
  const [busy, setBusy] = useState<"connect" | ClaudeTarget | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<ClaudeTarget[] | null>(null);
  const [allowWrites, setAllowWrites] = useState(false);

  const s = status.data;
  // Default selection: every target that is available and not yet connected.
  const selected =
    chosen ??
    new Set(TARGETS.filter((t) => s && (t.id === "desktop" || s[t.id].installed) && !s[t.id].connected).map((t) => t.id));
  const toggle = (id: ClaudeTarget) => {
    const next = new Set(selected);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setChosen(next);
  };

  async function connect() {
    const targets = TARGETS.filter((t) => selected.has(t.id) && (t.id === "desktop" || s?.[t.id].installed)).map(
      (t) => t.id,
    );
    setBusy("connect");
    setError(null);
    setDone(null);
    try {
      const next = await claude.connect(targets, orgId, ws.me.email, allowWrites);
      client.setQueryData(["claude-status"], next);
      setDone(targets);
      setChosen(new Set());
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(null);
    }
  }

  async function disconnect(target: ClaudeTarget) {
    setBusy(target);
    setError(null);
    try {
      client.setQueryData(["claude-status"], await claude.disconnect(target));
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(null);
    }
  }

  return (
    <Dialog title="Connect to Claude" onClose={busy ? () => {} : onClose} width={560}>
      <p className="mb-4 text-sm text-ink-2">
        Claude can search and read the pages of a workspace through MCP, from any project or chat. By default it is
        read-only; you can also let Claude save notes.
      </p>

      {status.isLoading && <Spinner />}
      {s && !s.mcp_binary && (
        <p className="mb-3 rounded-md bg-danger/10 px-3 py-2 text-sm text-danger">
          The MCP server is missing next to the app. Build it with <code>make desktop-deps</code>.
        </p>
      )}

      {s && (
        <div className="space-y-2">
          {TARGETS.map((t) => {
            const st = s[t.id];
            const usable = t.id === "desktop" || st.installed;
            const on = selected.has(t.id) && usable;
            return (
              <div
                key={t.id}
                onClick={() => usable && !busy && toggle(t.id)}
                className={cx(
                  "flex items-center gap-3 rounded-lg border px-3 py-3",
                  on ? "border-accent/60 bg-accent/5" : "border-line",
                  usable ? "hover:bg-hover" : "opacity-60",
                )}
              >
                <span
                  className={cx(
                    "flex h-4 w-4 shrink-0 items-center justify-center rounded border",
                    on ? "border-accent bg-accent text-white" : "border-ink-3",
                  )}
                >
                  {on && <Check size={12} strokeWidth={3} />}
                </span>
                <span className="text-ink-2">{t.icon}</span>
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2 text-sm font-medium">
                    {t.name}
                    <StatusChip status={st} serverUrl={serverUrl} />
                  </div>
                  {!st.installed && <div className="text-xs text-ink-3">{t.missing}</div>}
                </div>
                {st.connected &&
                  (busy === t.id ? (
                    <Spinner />
                  ) : (
                    <Button
                      variant="ghost"
                      className="h-7 px-2 text-xs"
                      disabled={busy !== null}
                      onClick={(e) => {
                        e.stopPropagation();
                        void disconnect(t.id);
                      }}
                    >
                      Disconnect
                    </Button>
                  ))}
              </div>
            );
          })}
        </div>
      )}

      {ws.orgs.length > 1 && (
        <label className="mt-4 flex items-center gap-2 text-sm text-ink-2">
          Workspace Claude searches by default
          <select
            value={orgId}
            onChange={(e) => setOrgId(e.target.value)}
            disabled={busy !== null}
            className="h-8 rounded-md bg-hover px-2 text-sm text-ink outline-none"
          >
            {ws.orgs.map((o) => (
              <option key={o.id} value={o.id}>
                {o.name}
              </option>
            ))}
          </select>
        </label>
      )}

      <label className="mt-4 flex cursor-default items-start gap-2.5 rounded-lg border border-line px-3 py-2.5 hover:bg-hover">
        <input
          type="checkbox"
          className="mt-0.5"
          checked={allowWrites}
          disabled={busy !== null}
          onChange={(e) => setAllowWrites(e.target.checked)}
        />
        <span>
          <span className="flex items-center gap-1.5 text-sm font-medium">
            <PenLine size={14} /> Allow Claude to create and edit pages
          </span>
          <span className="block text-xs text-ink-3">
            Ask Claude to “save this to nanace” or “add this to the Deploy Guide”. Claude writes as you, only in
            teamspaces where you can edit, and can never delete pages. Every change is in the audit log.
          </span>
        </span>
      </label>

      <div className="mt-3 flex items-start gap-2 text-xs text-ink-3">
        <ShieldCheck size={14} className="mt-px shrink-0" />
        <span>
          Each app gets its own access token, stored in its config. While this app runs, tokens are renewed every 7
          days automatically; an unused token expires after 30 days. Connecting asks you to sign in once and revokes
          earlier tokens; disconnecting revokes the token. The nanace server ({serverUrl}) must be running when Claude
          uses it.
        </span>
      </div>

      {error && <p className="mt-3 rounded-md bg-danger/10 px-3 py-2 text-sm text-danger selectable">{error}</p>}
      {done && (
        <div className="mt-3 rounded-md bg-[#0f7b6c]/10 px-3 py-2 text-sm">
          Connected. {TARGETS.filter((t) => done.includes(t.id)).map((t) => t.after).join(" ")}
          <div className="mt-1 text-ink-2">Try asking Claude: “Search nanace for our deployment checklist.”</div>
        </div>
      )}

      <div className="mt-5 flex items-center justify-end gap-2">
        {busy === "connect" && (
          <span className="mr-auto flex items-center gap-2 text-sm text-ink-2">
            <Spinner /> Signing in; confirm in the browser if it opens…
          </span>
        )}
        <Button onClick={onClose} disabled={busy !== null}>
          {done ? "Done" : "Cancel"}
        </Button>
        <Button
          variant="primary"
          disabled={busy !== null || selected.size === 0 || !s?.mcp_binary}
          onClick={() => void connect()}
        >
          Connect
        </Button>
      </div>
    </Dialog>
  );
}
