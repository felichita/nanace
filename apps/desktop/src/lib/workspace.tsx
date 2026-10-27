import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";

import { api, orgsOf, type ContextSummary, type Me, type Org, type Team } from "./api";

const ORG_KEY = "nanace.org";

// Module-level so TanStack Query memoizes the combined result between renders.
function combinePages(results: { data?: ContextSummary[]; isLoading: boolean }[]) {
  return { lists: results.map((r) => r.data), loading: results.some((r) => r.isLoading) };
}

function readStoredOrg(): string | null {
  try {
    return localStorage.getItem(ORG_KEY);
  } catch {
    return null;
  }
}

export interface Workspace {
  me: Me;
  orgs: Org[];
  org: Org;
  setOrgId(id: string): void;
  /** All teams in the org; `my_role` is null where the user is not a member. */
  teams: Team[];
  /** Teams the user belongs to. */
  myTeams: Team[];
  team(id: string): Team | undefined;
  /** Pages of the user's teams, keyed by id. */
  pages: Map<string, ContextSummary>;
  pagesOf(teamId: string): ContextSummary[];
  /** Lowercased title -> page id, for resolving `[[links]]`. */
  pageByTitle(title: string): string | undefined;
  loadingPages: boolean;
  /** Refetches page lists after a change. */
  refreshPages(teamId?: string): void;
}

const WorkspaceContext = createContext<Workspace | null>(null);

export function useWorkspace(): Workspace {
  const ws = useContext(WorkspaceContext);
  if (!ws) throw new Error("useWorkspace outside WorkspaceProvider");
  return ws;
}

export function WorkspaceProvider({ me, children }: { me: Me; children: (ws: Workspace | null) => ReactNode }) {
  const orgs = useMemo(() => orgsOf(me), [me]);
  const [orgId, setOrgIdState] = useState<string | null>(readStoredOrg);
  const org = orgs.find((o) => o.id === orgId) ?? orgs[0];
  const setOrgId = useCallback((id: string) => {
    setOrgIdState(id);
    try {
      localStorage.setItem(ORG_KEY, id);
    } catch {
      // Storage may be unavailable; the choice then lasts for this session.
    }
  }, []);

  const teamsQuery = useQuery({
    queryKey: ["teams", org?.id],
    queryFn: () => api.teams(org!.id),
    enabled: !!org,
  });
  const teams = useMemo(() => teamsQuery.data ?? [], [teamsQuery.data]);
  const myTeams = useMemo(() => teams.filter((t) => t.my_role), [teams]);

  const pageQueries = useQueries({
    queries: myTeams.map((t) => ({
      queryKey: ["pages", org?.id, t.id],
      queryFn: () => api.contexts(org!.id, t.id),
      enabled: !!org,
    })),
    combine: combinePages,
  });
  const pageLists = pageQueries.lists;
  const loadingPages = teamsQuery.isLoading || pageQueries.loading;

  const { pages, byTeam, byTitle } = useMemo(() => {
    const pages = new Map<string, ContextSummary>();
    const byTeam = new Map<string, ContextSummary[]>();
    const byTitle = new Map<string, string>();
    for (const list of pageLists) {
      for (const p of list ?? []) {
        pages.set(p.id, p);
        const arr = byTeam.get(p.team_id) ?? [];
        arr.push(p);
        byTeam.set(p.team_id, arr);
      }
    }
    for (const arr of byTeam.values()) arr.sort((a, b) => a.title.localeCompare(b.title));
    // Oldest first so that, for duplicate titles, the most recent page wins.
    const byAge = [...pages.values()].sort((a, b) => a.updated_at.localeCompare(b.updated_at));
    for (const p of byAge) byTitle.set(p.title.toLowerCase(), p.id);
    return { pages, byTeam, byTitle };
  }, [pageLists]);

  const client = useQueryClient();
  const refreshPages = useCallback(
    (teamId?: string) => {
      void client.invalidateQueries({ queryKey: teamId ? ["pages", org?.id, teamId] : ["pages", org?.id] });
      void client.invalidateQueries({ queryKey: ["graph", org?.id] });
    },
    [client, org?.id],
  );

  useEffect(() => {
    if (org && org.id !== orgId) setOrgId(org.id);
  }, [org, orgId, setOrgId]);

  const ws: Workspace | null = org
    ? {
        me,
        orgs,
        org,
        setOrgId,
        teams,
        myTeams,
        team: (id) => teams.find((t) => t.id === id),
        pages,
        pagesOf: (id) => byTeam.get(id) ?? [],
        pageByTitle: (title) => byTitle.get(title.trim().toLowerCase()),
        loadingPages,
        refreshPages,
      }
    : null;

  return <WorkspaceContext.Provider value={ws}>{children(ws)}</WorkspaceContext.Provider>;
}
