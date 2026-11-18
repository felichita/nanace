import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowDownLeft, ArrowUpRight, Octagon, Radio } from "lucide-react";
import { useState } from "react";

import { api, type Team } from "../lib/api";
import {
  ACCEPT_MODE_LABEL,
  allowedModes,
  p2p,
  type AcceptMode,
  type P2pSettings,
  type TeamPolicy,
  type WhoAmI,
} from "../lib/p2p";
import { useWorkspace } from "../lib/workspace";
import { Button, cx, Dialog, Spinner, TeamIcon } from "./ui";

/** A select value for "this machine takes no part in the team". */
const NOT_TAKING_PART = "none";

function TeamRow({
  team,
  policy,
  settings,
  view,
  onChanged,
}: {
  team: Team;
  policy: TeamPolicy | undefined;
  settings: P2pSettings | undefined;
  view: WhoAmI["teams"][number] | undefined;
  onChanged: () => void;
}) {
  const ws = useWorkspace();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const mine = settings?.teams.find((t) => t.team_id === team.id);
  const modes = allowedModes(policy?.enabled ? policy.policy?.max_accept_mode : undefined);
  const owner = team.my_role === "admin";

  const run = async (action: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await action();
      onChanged();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  // Only the teamspace owner turns P2P on, by creating the team's policy page.
  const createPolicy = () =>
    run(async () => {
      const template = await api.p2pPolicyTemplate();
      await api.createContext(ws.org.id, team.id, template.title, template.content, ["policy"]);
      ws.refreshPages(team.id);
    });

  return (
    <div className="rounded-lg border border-line px-3 py-2.5">
      <div className="flex items-center gap-2">
        <TeamIcon id={team.id} name={team.name} size={18} />
        <span className="min-w-0 flex-1 truncate text-sm font-medium">{team.name}</span>
        {busy && <Spinner />}
        {policy?.enabled ? (
          <select
            disabled={busy}
            value={mine ? mine.accept_mode : NOT_TAKING_PART}
            onChange={(e) =>
              void run(() =>
                e.target.value === NOT_TAKING_PART
                  ? p2p.leaveTeam(team.id)
                  : p2p.setTeam(ws.org.id, team.id, e.target.value as AcceptMode),
              )
            }
            className="h-7 max-w-[260px] rounded-md border border-line bg-transparent px-1 text-[13px]"
            aria-label={`P2P in ${team.name}`}
          >
            <option value={NOT_TAKING_PART}>Not taking part</option>
            {modes.map((mode) => (
              <option key={mode} value={mode}>
                {ACCEPT_MODE_LABEL[mode]}
              </option>
            ))}
          </select>
        ) : (
          <span className="text-xs text-ink-3">P2P is off in this team</span>
        )}
      </div>
      {policy && !policy.enabled && (
        <p className="mt-1.5 text-xs text-ink-3">
          {policy.errors[0] ?? "The team's policy turns P2P off."}{" "}
          {owner && !policy.version ? (
            <button className="text-accent hover:underline" disabled={busy} onClick={() => void createPolicy()}>
              Create "Policy: P2P" from the template
            </button>
          ) : (
            !owner && "Only the teamspace owner can change this."
          )}
        </p>
      )}
      {mine && view && view.effective_accept_mode !== mine.accept_mode && (
        <p className="mt-1.5 text-xs text-ink-3">
          In effect now: {ACCEPT_MODE_LABEL[view.effective_accept_mode].toLowerCase()}.
        </p>
      )}
      {mine && Object.keys(mine.trusted).length > 0 && (
        <TrustedList team={team} trusted={mine.trusted} onChanged={onChanged} />
      )}
      {error && <p className="mt-1.5 text-xs text-danger">{error}</p>}
    </div>
  );
}

function TrustedList({
  team,
  trusted,
  onChanged,
}: {
  team: Team;
  trusted: Record<string, number>;
  onChanged: () => void;
}) {
  const ws = useWorkspace();
  const members = useQuery({ queryKey: ["members", ws.org.id, team.id], queryFn: () => api.members(ws.org.id, team.id) });
  const now = Date.now() / 1000;
  const current = Object.entries(trusted).filter(([, until]) => until > now);
  if (current.length === 0) return null;
  return (
    <div className="mt-2 border-t border-line pt-2 text-xs text-ink-2">
      Trusted:
      {current.map(([userId, until]) => (
        <span key={userId} className="ml-1.5 inline-flex items-center gap-1 rounded bg-chip px-1.5 py-0.5">
          {members.data?.find((m) => m.user_id === userId)?.email ?? "a teammate"}
          {/* A far-future time means "until I remove them". */}
          {until < now + 10 * 365 * 86400 && (
            <span className="text-ink-3">until {new Date(until * 1000).toLocaleTimeString()}</span>
          )}
          <button
            className="text-ink-3 hover:text-danger"
            aria-label="Stop trusting"
            onClick={() => void p2p.setTrusted(team.id, userId, false).then(onChanged)}
          >
            ×
          </button>
        </span>
      ))}
    </div>
  );
}

const STATE_LABEL: Record<string, string> = {
  queued: "waiting to be delivered",
  delivered: "delivered, no answer yet",
  answered: "answered",
  rejected: "refused",
  expired: "expired",
  failed: "failed",
  pending_consent: "waiting for you",
  inbox: "with your agent",
  pending_review: "answer waiting for you",
  answer_queued: "answer being sent",
  done: "done",
};

function ActivityView() {
  const activity = useQuery({ queryKey: ["p2p-activity"], queryFn: p2p.activity, refetchInterval: 3000 });
  const a = activity.data;
  if (!a) return activity.isLoading ? <Spinner /> : <p className="text-sm text-ink-3">No activity yet.</p>;
  const rows = [
    ...a.outgoing.map((o) => ({
      id: o.request_id,
      out: true,
      who: o.to_agent,
      kind: o.kind,
      state: o.reason ? `${STATE_LABEL[o.state]} (${o.reason})` : STATE_LABEL[o.state],
      trace: o.trace_id,
    })),
    ...a.incoming.map((i) => ({
      id: i.request_id,
      out: false,
      who: i.from_email,
      kind: i.kind,
      state: STATE_LABEL[i.state],
      trace: i.trace_id,
    })),
  ];
  return (
    <div className="space-y-4 text-sm">
      <div>
        <div className="mb-1 text-xs font-medium text-ink-3">Connections</div>
        {a.connections.length === 0 ? (
          <p className="text-ink-3">Not connected to any peer right now.</p>
        ) : (
          a.connections.map((c) => (
            <div key={c.peer_id} className="flex items-center gap-2 py-0.5">
              <Radio size={13} className="text-ink-3" />
              <code className="truncate text-xs">{c.peer_id}</code>
              <span className="text-xs text-ink-3">{c.relayed ? "through the relay" : "direct"}</span>
            </div>
          ))
        )}
      </div>
      <div>
        <div className="mb-1 text-xs font-medium text-ink-3">Recent exchanges (what happened, never what was said)</div>
        {rows.length === 0 ? (
          <p className="text-ink-3">Nothing was sent or received in the last day.</p>
        ) : (
          rows.map((r) => (
            <div key={`${r.out}-${r.id}`} className="flex items-center gap-2 py-0.5">
              {r.out ? (
                <ArrowUpRight size={13} className="text-ink-3" />
              ) : (
                <ArrowDownLeft size={13} className="text-ink-3" />
              )}
              <span className="w-12 text-xs text-ink-3">{r.kind}</span>
              <span className="min-w-0 flex-1 truncate">{r.who}</span>
              <span className="text-xs text-ink-2">{r.state}</span>
              <code className="text-xs text-ink-3">{r.trace}</code>
            </div>
          ))
        )}
      </div>
    </div>
  );
}

/** Settings and activity of agent-to-agent messaging on this machine. */
export function P2PDialog({ onClose }: { onClose: () => void }) {
  const ws = useWorkspace();
  const client = useQueryClient();
  const [tab, setTab] = useState<"settings" | "activity">("settings");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const state = useQuery({ queryKey: ["p2p-state"], queryFn: p2p.state, refetchInterval: 5000 });
  const running = state.data?.running ?? false;
  const whoami = useQuery({ queryKey: ["p2p-whoami"], queryFn: p2p.whoami, enabled: running, refetchInterval: 5000 });
  const settings = useQuery({ queryKey: ["p2p-settings"], queryFn: p2p.settings, enabled: running });
  const policies = useQueries({
    queries: ws.myTeams.map((t) => ({
      queryKey: ["p2p-policy", ws.org.id, t.id],
      queryFn: () => api.p2pPolicy(ws.org.id, t.id),
    })),
  });

  const refresh = () => {
    for (const key of ["p2p-state", "p2p-whoami", "p2p-settings", "p2p-policy", "p2p-pending"]) {
      void client.invalidateQueries({ queryKey: [key] });
    }
  };
  const run = async (action: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
      refresh();
    }
  };

  const s = state.data;
  return (
    <Dialog title="Agents (P2P)" onClose={busy ? () => {} : onClose} width={620}>
      <p className="mb-4 text-sm text-ink-2">
        Lets Claude on this computer ask and answer questions with your teammates' agents. Messages go between the
        computers; nanace records who talked to whom, never what was said. Nothing is accepted unless this app is
        running and you allowed it.
      </p>
      {state.isLoading && <Spinner />}
      {s && !s.available && (
        <p className="mb-3 rounded-md bg-danger/10 px-3 py-2 text-sm text-danger">
          The P2P daemon is missing next to the app. Build it with <code>make desktop-deps</code>.
        </p>
      )}
      {s && s.available && !s.node_configured && (
        <p className="mb-3 rounded-md bg-chip px-3 py-2 text-sm text-ink-2">
          This server has no P2P network node, so P2P cannot be turned on. Ask its administrator.
        </p>
      )}
      {(error ?? s?.error) && (
        <p className="mb-3 rounded-md bg-danger/10 px-3 py-2 text-sm text-danger">{error ?? s?.error}</p>
      )}

      {s && s.available && s.node_configured && (
        <div className="mb-4 flex items-center gap-3 rounded-lg border border-line px-3 py-3">
          <div className="min-w-0 flex-1">
            <div className="text-sm font-medium">
              {s.enabled ? (running ? "P2P is on" : "P2P is on, starting…") : "P2P is off"}
            </div>
            <div className="truncate text-xs text-ink-3">
              {s.enabled && whoami.data
                ? `Peer ${whoami.data.peer_id.slice(0, 16)}… · ${whoami.data.agents.length} agent session(s) connected`
                : "Turning it on asks you to sign in again and gives the daemon its own, limited token."}
            </div>
          </div>
          {busy && <Spinner />}
          {s.enabled ? (
            <Button disabled={busy} onClick={() => void run(p2p.disable)}>
              Turn off
            </Button>
          ) : (
            <Button variant="primary" disabled={busy} onClick={() => void run(() => p2p.enable(ws.me.email))}>
              Turn on P2P
            </Button>
          )}
        </div>
      )}

      {s?.enabled && running && (
        <>
          <div className="mb-3 flex items-center gap-1 border-b border-line">
            {(["settings", "activity"] as const).map((id) => (
              <button
                key={id}
                onClick={() => setTab(id)}
                className={cx(
                  "border-b-2 px-3 py-1.5 text-sm capitalize",
                  tab === id ? "border-accent font-medium" : "border-transparent text-ink-2 hover:text-ink",
                )}
              >
                {id}
              </button>
            ))}
            <span className="flex-1" />
            <Button
              variant="danger"
              className="mb-1 h-7"
              disabled={busy}
              title="Refuse everything that is waiting, disappear from every team and close all connections"
              onClick={() => void run(p2p.stopAll)}
            >
              <Octagon size={13} /> Stop everything
            </Button>
          </div>
          {tab === "activity" ? (
            <ActivityView />
          ) : (
            <div className="space-y-2">
              {ws.myTeams.map((team, i) => (
                <TeamRow
                  key={team.id}
                  team={team}
                  policy={policies[i]?.data}
                  settings={settings.data}
                  view={whoami.data?.teams.find((t) => t.team_id === team.id)}
                  onChanged={refresh}
                />
              ))}
              <label className="flex items-start gap-2 pt-2 text-sm">
                <input
                  type="checkbox"
                  className="mt-0.5"
                  checked={settings.data?.review_answers ?? false}
                  disabled={busy || !settings.data}
                  onChange={(e) => void run(() => p2p.setReviewAnswers(e.target.checked))}
                />
                <span>
                  Show me each answer before it is sent
                  <span className="block text-xs text-ink-3">
                    Your agent's answer waits until you confirm it. Secrets are blocked either way.
                  </span>
                </span>
              </label>
              <p className="pt-1 text-xs text-ink-3">
                Claude gets the P2P tools the next time you use "Connect to Claude".
              </p>
            </div>
          )}
        </>
      )}
    </Dialog>
  );
}
