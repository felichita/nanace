import { useQuery, useQueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";

import { p2p, P2P_NOTICE_EVENT, type Pending } from "../lib/p2p";
import { useWorkspace } from "../lib/workspace";
import { Button, Dialog } from "./ui";

const TRUST_HOURS = [1, 8, 24];

function timeLeft(deadline: number): string {
  const minutes = Math.max(0, Math.round((deadline * 1000 - Date.now()) / 60000));
  return minutes >= 120 ? `${Math.round(minutes / 60)} hours` : `${minutes} min`;
}

/** Text a peer wrote: shown as plain text, never rendered as markup. */
function PeerText({ label, text }: { label: string; text: string }) {
  return (
    <div className="mb-3">
      <div className="mb-1 text-xs font-medium text-ink-3">{label}</div>
      <pre className="max-h-[32vh] overflow-auto whitespace-pre-wrap break-words rounded-md bg-chip px-3 py-2 font-sans text-sm">
        {text || "(empty)"}
      </pre>
    </div>
  );
}

/**
 * The consent window: a request from another agent waits here until the user
 * allows or denies it, and an agent's answer waits here before it is sent.
 * Shown over whatever the user is doing; closing it leaves the request
 * waiting until it expires.
 */
export function P2PPrompts({ running }: { running: boolean }) {
  const ws = useWorkspace();
  const client = useQueryClient();
  const [hidden, setHidden] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const pending = useQuery({
    queryKey: ["p2p-pending"],
    queryFn: p2p.pending,
    enabled: running,
    refetchInterval: 5000,
  });

  // A notice means something new waits: fetch it at once and show it again
  // even if the user closed an earlier prompt.
  useEffect(() => {
    const unlisten = listen<{ event: string }>(P2P_NOTICE_EVENT, (event) => {
      if (event.payload.event !== "activity") setHidden(null);
      void client.invalidateQueries({ queryKey: ["p2p-pending"] });
      void client.invalidateQueries({ queryKey: ["p2p-activity"] });
    });
    return () => void unlisten.then((stop) => stop());
  }, [client]);

  const current: Pending | undefined = running ? pending.data?.[0] : undefined;
  if (!current || current.request.request_id === hidden) return null;
  const { request, answer } = current;
  const team = ws.team(request.team_id)?.name ?? "a team you are in";
  const more = (pending.data?.length ?? 1) - 1;

  const decide = async (run: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await run();
      await client.invalidateQueries({ queryKey: ["p2p-pending"] });
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const review = answer !== null;
  return (
    <Dialog
      title={review ? "Send this answer?" : "An agent asks for your agent's help"}
      onClose={() => setHidden(request.request_id)}
      width={560}
    >
      <p className="mb-3 text-sm text-ink-2">
        <span className="font-medium text-ink">{request.from_email}</span> (agent <code>{request.from_agent}</code>) in{" "}
        <span className="font-medium text-ink">{team}</span>
        {review
          ? " asked the question below. Your agent wrote an answer; it is sent only if you agree."
          : request.kind === "status"
            ? " wants to see your agent's status."
            : request.kind === "notify"
              ? " sent a notification to your agent."
              : " sent a question to your agent."}
      </p>
      <PeerText label={request.kind === "notify" ? "Notification" : "Question"} text={request.body} />
      {review && <PeerText label={`Answer from your agent (${request.to_agent})`} text={answer} />}
      <p className="mb-4 text-xs text-ink-3">
        Task <code>{request.trace_id}</code>
        {request.refs.length > 0 && ` · refers to ${request.refs.length} page(s)`} · expires in{" "}
        {timeLeft(request.deadline)}
        {more > 0 && ` · ${more} more waiting`}
      </p>
      {error && <p className="mb-3 rounded-md bg-danger/10 px-3 py-2 text-sm text-danger">{error}</p>}
      {review ? (
        <div className="flex justify-end gap-2">
          <Button disabled={busy} onClick={() => void decide(() => p2p.decideReview(request.request_id, false))}>
            Don't send
          </Button>
          <Button
            variant="primary"
            disabled={busy}
            onClick={() => void decide(() => p2p.decideReview(request.request_id, true))}
          >
            Send answer
          </Button>
        </div>
      ) : (
        <div className="flex flex-wrap items-center justify-end gap-2">
          <Button
            variant="danger"
            disabled={busy}
            onClick={() => void decide(() => p2p.decideConsent(request.request_id, false))}
          >
            Deny
          </Button>
          <select
            disabled={busy}
            defaultValue=""
            onChange={(e) => {
              const hours = Number(e.target.value);
              if (hours) void decide(() => p2p.decideConsent(request.request_id, true, hours));
            }}
            className="h-8 rounded-md border border-line bg-transparent px-2 text-sm"
            aria-label="Allow this person for a while"
          >
            <option value="">Allow this person for…</option>
            {TRUST_HOURS.map((h) => (
              <option key={h} value={h}>
                {h} {h === 1 ? "hour" : "hours"}
              </option>
            ))}
          </select>
          <Button
            variant="primary"
            disabled={busy}
            onClick={() => void decide(() => p2p.decideConsent(request.request_id, true))}
          >
            Allow
          </Button>
        </div>
      )}
    </Dialog>
  );
}
