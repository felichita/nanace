// Typed access to the nanace REST API through the Tauri `api` command. The
// Rust side attaches the token; the webview never sees it.

import { invoke } from "@tauri-apps/api/core";

export type TeamRole = "reader" | "writer" | "admin";
export type OrgRole = "member" | "admin";

export interface SessionInfo {
  server_url: string;
  signed_in: boolean;
}

export interface Membership {
  org_id: string;
  org_slug: string;
  org_name: string;
  org_role: OrgRole;
  team_id: string | null;
  team_slug: string | null;
  team_role: TeamRole | null;
}

export interface Me {
  user_id: string;
  email: string;
  memberships: Membership[];
}

export interface Org {
  id: string;
  slug: string;
  name: string;
  role: OrgRole;
}

export interface Team {
  id: string;
  slug: string;
  name: string;
  my_role: TeamRole | null;
}

export interface ContextSummary {
  id: string;
  team_id: string;
  title: string;
  tags: string[];
  updated_by: string;
  updated_at: string;
}

export interface Context extends ContextSummary {
  org_id: string;
  content: string;
  created_by: string;
  created_at: string;
}

export interface LinkedContext {
  id: string;
  team_id: string;
  title: string;
}

export interface ContextLinks {
  outgoing: LinkedContext[];
  backlinks: LinkedContext[];
  related: (LinkedContext & { score: number })[];
}

export interface GraphNode {
  id: string;
  team_id: string;
  title: string;
  tags: string[];
  updated_at: string;
}

export interface GraphEdge {
  source: string;
  target: string;
  kind: "link" | "semantic";
  score?: number;
}

export interface Graph {
  nodes: GraphNode[];
  edges: GraphEdge[];
  truncated: boolean;
}

export interface SearchHit {
  context_id: string;
  team_id: string;
  title: string;
  tags: string[];
  score: number;
  snippet: string;
  updated_at: string;
}

export interface TrashItem {
  id: string;
  team_id: string;
  title: string;
  tags: string[];
  deleted_at: string;
  deleted_by: string;
  deleted_by_email: string;
  /** When the page is deleted for good; null when trash is kept. */
  purge_at: string | null;
}

export interface Member {
  user_id: string;
  email: string;
  display_name: string | null;
  role: TeamRole;
  source: string;
}

export interface OrgMember {
  user_id: string;
  email: string;
  display_name: string | null;
  role: OrgRole;
  teams: string[];
}

export interface AuditEntry {
  id: number;
  actor_user_id: string | null;
  actor_email: string | null;
  action: string;
  resource_type: string;
  resource_id: string | null;
  details: unknown;
  created_at: string;
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

/** Fired when the server rejects the stored token; the app returns to sign-in. */
export const SIGNED_OUT_EVENT = "nanace:signed-out";

interface RawResponse {
  status: number;
  body: unknown;
}

async function call<T>(method: string, path: string, body?: unknown): Promise<T> {
  const resp = await invoke<RawResponse>("api", { method, path: `/api/v1${path}`, body: body ?? null });
  if (resp.status === 401) {
    window.dispatchEvent(new Event(SIGNED_OUT_EVENT));
  }
  if (resp.status >= 400) {
    const message =
      typeof resp.body === "object" && resp.body && "message" in resp.body
        ? String((resp.body as { message: unknown }).message)
        : `Request failed (${resp.status})`;
    throw new ApiError(resp.status, message);
  }
  return resp.body as T;
}

const q = (params: Record<string, string | number | boolean | undefined>) => {
  const s = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v !== undefined) s.set(k, String(v));
  const out = s.toString();
  return out ? `?${out}` : "";
};

const PAGE_SIZE = 200;

export const api = {
  me: () => call<Me>("GET", "/me"),
  createOrg: (slug: string, name: string) => call<{ id: string }>("POST", "/orgs", { slug, name }),
  teams: (org: string) => call<Team[]>("GET", `/orgs/${org}/teams`),
  createTeam: (org: string, slug: string, name: string) =>
    call<Team>("POST", `/orgs/${org}/teams`, { slug, name }),
  members: (org: string, team: string) => call<Member[]>("GET", `/orgs/${org}/teams/${team}/members`),
  putMember: (org: string, team: string, email: string, role: TeamRole) =>
    call<unknown>("PUT", `/orgs/${org}/teams/${team}/members`, { email, role }),
  removeMember: (org: string, team: string, user: string) =>
    call<unknown>("DELETE", `/orgs/${org}/teams/${team}/members/${user}`),
  orgMembers: (org: string) => call<OrgMember[]>("GET", `/orgs/${org}/members`),
  removeOrgMember: (org: string, user: string) => call<null>("DELETE", `/orgs/${org}/members/${user}`),
  renameOrg: (org: string, name: string) => call<Org>("PATCH", `/orgs/${org}`, { name }),
  deleteOrg: (org: string, confirm: string) => call<null>("DELETE", `/orgs/${org}`, { confirm }),
  auditLogs: (org: string, limit: number, beforeId?: number) =>
    call<AuditEntry[]>("GET", `/orgs/${org}/audit-logs${q({ limit, before_id: beforeId })}`),
  setOrgRole: (org: string, user: string, role: OrgRole) =>
    call<null>("PUT", `/orgs/${org}/members/${user}`, { role }),

  /** Every context of a team, following pagination. */
  async contexts(org: string, team: string): Promise<ContextSummary[]> {
    const all: ContextSummary[] = [];
    for (let offset = 0; ; offset += PAGE_SIZE) {
      const page = await call<ContextSummary[]>(
        "GET",
        `/orgs/${org}/teams/${team}/contexts${q({ limit: PAGE_SIZE, offset })}`,
      );
      all.push(...page);
      if (page.length < PAGE_SIZE) return all;
    }
  },
  context: (org: string, id: string) => call<Context>("GET", `/orgs/${org}/contexts/${id}`),
  createContext: (org: string, team: string, title: string, content = "", tags: string[] = []) =>
    call<Context>("POST", `/orgs/${org}/teams/${team}/contexts`, { title, content, tags }),
  updateContext: (
    org: string,
    id: string,
    input: { title: string; content: string; tags: string[]; base_updated_at: string },
  ) => call<Context>("PUT", `/orgs/${org}/contexts/${id}`, input),
  deleteContext: (org: string, id: string) => call<null>("DELETE", `/orgs/${org}/contexts/${id}`),
  trash: (org: string) => call<TrashItem[]>("GET", `/orgs/${org}/trash`),
  restore: (org: string, id: string) => call<Context>("POST", `/orgs/${org}/trash/${id}/restore`),
  purge: (org: string, id: string) => call<null>("DELETE", `/orgs/${org}/trash/${id}`),
  links: (org: string, id: string) => call<ContextLinks>("GET", `/orgs/${org}/contexts/${id}/links`),
  graph: (org: string, params: { team_id?: string; min_score?: number; neighbors?: number }) =>
    call<Graph>("GET", `/orgs/${org}/graph${q(params)}`),
  search: (org: string, query: string, limit = 8) =>
    call<SearchHit[]>("POST", `/orgs/${org}/search`, { query, limit }),
};

export type ClaudeTarget = "code" | "desktop";

export interface ClaudeTargetStatus {
  installed: boolean;
  connected: boolean;
  api_url: string | null;
  writes: boolean;
  /** Unix seconds of the last token issue or rotation by this app. */
  rotated_at: number | null;
}

/** Sent by the app when a Claude connection's token can no longer be renewed. */
export const CLAUDE_EXPIRED_EVENT = "claude-connection-expired";

export interface ClaudeStatus {
  code: ClaudeTargetStatus;
  desktop: ClaudeTargetStatus;
  mcp_binary: string | null;
}

export const claude = {
  status: () => invoke<ClaudeStatus>("claude_status"),
  connect: (targets: ClaudeTarget[], orgId: string, email: string, allowWrites: boolean) =>
    invoke<ClaudeStatus>("connect_claude", { targets, orgId, email, allowWrites }),
  disconnect: (target: ClaudeTarget) => invoke<ClaudeStatus>("disconnect_claude", { target }),
};

export const session = {
  info: () => invoke<SessionInfo>("session_info"),
  setServerUrl: (url: string) => invoke<SessionInfo>("set_server_url", { url }),
  signInGoogle: () => invoke<SessionInfo>("sign_in_google"),
  signInDev: (email: string) => invoke<SessionInfo>("sign_in_dev", { email }),
  signOut: () => invoke<SessionInfo>("sign_out"),
  openExternal: (url: string) => invoke<void>("open_external", { url }),
  /** Erases the account after a fresh sign-in; the app is signed out afterwards. */
  deleteAccount: (email: string) => invoke<SessionInfo>("delete_account", { email }),
};

export function orgsOf(me: Me): Org[] {
  const orgs = new Map<string, Org>();
  for (const m of me.memberships) {
    orgs.set(m.org_id, { id: m.org_id, slug: m.org_slug, name: m.org_name, role: m.org_role });
  }
  return [...orgs.values()].sort((a, b) => a.name.localeCompare(b.name));
}

export const canWrite = (role: TeamRole | null | undefined) => role === "writer" || role === "admin";

export function slugify(name: string): string {
  const slug = name
    .toLowerCase()
    .normalize("NFKD")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 40);
  return slug.length >= 2 ? slug : `${slug || "ws"}-${Math.random().toString(36).slice(2, 6)}`;
}
