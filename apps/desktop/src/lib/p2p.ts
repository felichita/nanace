// The P2P daemon as the app's pages see it. Calls go through the shell's
// `p2p_call` command, which allows a fixed set of methods; the peer key, the
// daemon's token and its secrets never reach this code.
import { invoke } from "@tauri-apps/api/core";

/** Carries a daemon notice (consent or review request) to the UI. */
export const P2P_NOTICE_EVENT = "p2p-notice";
/** Sent when the daemon's token could not be renewed and P2P stopped. */
export const P2P_EXPIRED_EVENT = "p2p-token-expired";

export type AcceptMode = "off" | "ask" | "trusted" | "team";
export const ACCEPT_MODES: AcceptMode[] = ["off", "ask", "trusted", "team"];

export const ACCEPT_MODE_LABEL: Record<AcceptMode, string> = {
  off: "Do not accept requests",
  ask: "Ask me every time",
  trusted: "Accept from people I trust, ask for others",
  team: "Accept from the whole team",
};

/** Modes a member may choose: everything up to what the team policy allows. */
export function allowedModes(max: AcceptMode | undefined): AcceptMode[] {
  return ACCEPT_MODES.slice(0, ACCEPT_MODES.indexOf(max ?? "off") + 1);
}

export interface P2pState {
  available: boolean;
  enabled: boolean;
  running: boolean;
  node_configured: boolean;
  error: string | null;
}

export interface P2pTeamView {
  org_id: string;
  team_id: string;
  team_slug: string | null;
  accept_mode: AcceptMode;
  effective_accept_mode: AcceptMode;
  p2p_enabled: boolean;
}

export interface WhoAmI {
  peer_id: string;
  email: string;
  agents: string[];
  teams: P2pTeamView[];
}

export interface P2pSettings {
  teams: { org_id: string; team_id: string; accept_mode: AcceptMode; trusted: Record<string, number> }[];
  review_answers: boolean;
}

export type MessageKind = "ask" | "status" | "notify";

/** A request from another agent, as the daemon describes it. */
export interface InboxItem {
  request_id: string;
  trace_id: string;
  org_id: string;
  team_id: string;
  kind: MessageKind;
  state: "pending_consent" | "inbox" | "pending_review" | "answer_queued" | "done";
  from_peer: string;
  from_agent: string;
  from_user: string;
  /** From the registry, not from the message. */
  from_email: string;
  to_agent: string;
  body: string;
  refs: string[];
  /** Unix seconds. */
  deadline: number;
  received_at: number;
}

/** A request waiting for the user: consent, or review of the agent's answer. */
export interface Pending {
  request: InboxItem;
  /** The agent's answer, when it waits for review. */
  answer: string | null;
}

export interface Outgoing {
  request_id: string;
  trace_id: string;
  kind: MessageKind;
  state: "queued" | "delivered" | "answered" | "rejected" | "expired" | "failed";
  reason: string | null;
  to_agent: string;
}

export interface Activity {
  connections: { peer_id: string; relayed: boolean }[];
  outgoing: Outgoing[];
  incoming: InboxItem[];
}

export interface TeamPolicy {
  version: string | null;
  valid: boolean;
  enabled: boolean;
  policy: { max_accept_mode: AcceptMode } | null;
  errors: string[];
}

const daemon = <T,>(method: string, params: Record<string, unknown> = {}) =>
  invoke<T>("p2p_call", { method, params });

export const p2p = {
  state: () => invoke<P2pState>("p2p_state"),
  /** Needs a fresh sign-in; `email` is used only with dev login. */
  enable: (email: string) => invoke<P2pState>("p2p_enable", { email }),
  disable: () => invoke<P2pState>("p2p_disable"),
  whoami: () => daemon<WhoAmI>("whoami"),
  settings: () => daemon<P2pSettings>("settings"),
  activity: () => daemon<Activity>("activity"),
  pending: () => daemon<Pending[]>("pending"),
  setTeam: (orgId: string, teamId: string, mode: AcceptMode) =>
    daemon<unknown>("set_team", { org_id: orgId, team_id: teamId, accept_mode: mode }),
  leaveTeam: (teamId: string) => daemon<unknown>("leave_team", { team_id: teamId }),
  setTrusted: (teamId: string, userId: string, trusted: boolean) =>
    daemon<unknown>("set_trusted", { team_id: teamId, user_id: userId, trusted }),
  setReviewAnswers: (review: boolean) => daemon<unknown>("set_review_answers", { review }),
  decideConsent: (requestId: string, allow: boolean, trustHours?: number) =>
    daemon<unknown>("decide_consent", { request_id: requestId, allow, trust_hours: trustHours ?? null }),
  decideReview: (requestId: string, allow: boolean) =>
    daemon<unknown>("decide_review", { request_id: requestId, allow }),
  stopAll: () => daemon<unknown>("stop_all"),
};
