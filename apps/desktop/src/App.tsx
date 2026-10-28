import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Route, Switch, useLocation } from "wouter";

import { CommandPalette } from "./components/CommandPalette";
import { ConnectClaudeDialog } from "./components/ConnectClaudeDialog";
import { GraphView } from "./components/GraphView";
import { Home } from "./components/Home";
import { ImportMemoryDialog } from "./components/ImportMemoryDialog";
import { MembersDialog, TeamsDialog } from "./components/MembersDialog";
import { PageView } from "./components/PageView";
import { Sidebar } from "./components/Sidebar";
import { Toast, TrashDialog } from "./components/Trash";
import { DeleteAccountDialog } from "./components/WorkspaceSettings";
import { Onboarding, SignIn } from "./components/SignIn";
import { Spinner } from "./components/ui";
import { listen } from "@tauri-apps/api/event";

import { api, canWrite, CLAUDE_EXPIRED_EVENT, session, SIGNED_OUT_EVENT, type SessionInfo, type Team } from "./lib/api";
import { LayoutContext, type Layout } from "./lib/layout";
import { WorkspaceProvider, type Workspace } from "./lib/workspace";

const SIDEBAR_KEY = "nanace.sidebar";

function readSidebar(): { open: boolean; width: number } {
  try {
    const v = JSON.parse(localStorage.getItem(SIDEBAR_KEY) ?? "null") as { open: boolean; width: number } | null;
    if (v && typeof v.open === "boolean" && typeof v.width === "number") return v;
  } catch {
    // Fall through to defaults.
  }
  return { open: true, width: 248 };
}

function Center({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex h-full items-center justify-center" data-tauri-drag-region>
      {children}
    </div>
  );
}

/** Opens external links in the system browser instead of the webview. */
function useExternalLinks() {
  useEffect(() => {
    const onClick = (e: MouseEvent) => {
      const a = (e.target as HTMLElement | null)?.closest?.("a[href]") as HTMLAnchorElement | null;
      if (!a) return;
      const href = a.getAttribute("href") ?? "";
      if (/^(https?:|mailto:)/i.test(href)) {
        e.preventDefault();
        e.stopPropagation();
        void session.openExternal(href);
      }
    };
    document.addEventListener("click", onClick, true);
    return () => document.removeEventListener("click", onClick, true);
  }, []);
}

function Shell({ ws, info, onSignOut }: { ws: Workspace; info: SessionInfo; onSignOut: () => void }) {
  const [sidebar, setSidebar] = useState(readSidebar);
  const [palette, setPalette] = useState(false);
  const [membersOf, setMembersOf] = useState<Team | null>(null);
  const [teamsOpen, setTeamsOpen] = useState(false);
  const [importOpen, setImportOpen] = useState(false);
  const [trashOpen, setTrashOpen] = useState(false);
  const [connectOpen, setConnectOpen] = useState(false);
  const [deleteAccountOpen, setDeleteAccountOpen] = useState(false);
  const [toast, setToast] = useState<{ message: string; action?: { label: string; run: () => void } } | null>(null);
  const toastTimer = useRef<number | undefined>(undefined);
  const [location, navigate] = useLocation();
  const client = useQueryClient();
  const resizing = useRef(false);

  useEffect(() => {
    try {
      localStorage.setItem(SIDEBAR_KEY, JSON.stringify(sidebar));
    } catch {
      // Not persisted; fine.
    }
  }, [sidebar]);

  const currentTeam = useMemo(() => {
    const m = /^\/page\/(.+)$/.exec(location);
    return m ? ws.pages.get(m[1])?.team_id : undefined;
  }, [location, ws.pages]);

  const newPage = useCallback(
    async (teamId?: string, title?: string) => {
      const writable = ws.myTeams.filter((t) => canWrite(t.my_role));
      const target =
        writable.find((t) => t.id === teamId) ?? writable.find((t) => t.id === currentTeam) ?? writable[0];
      if (!target) return null;
      const page = await api.createContext(ws.org.id, target.id, title?.trim() || "Untitled");
      client.setQueryData(["page", ws.org.id, page.id], page);
      ws.refreshPages(target.id);
      navigate(`/page/${page.id}`);
      return page.id;
    },
    [client, currentTeam, navigate, ws],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.metaKey && !e.ctrlKey) return;
      if (e.key === "k" || e.key === "p") {
        e.preventDefault();
        setPalette((v) => !v);
      } else if (e.key === "\\") {
        e.preventDefault();
        setSidebar((s) => ({ ...s, open: !s.open }));
      } else if (e.key === "n") {
        e.preventDefault();
        void newPage();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [newPage]);

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (resizing.current) setSidebar((s) => ({ ...s, width: Math.min(420, Math.max(200, e.clientX)) }));
    };
    const onUp = () => {
      resizing.current = false;
      document.body.style.cursor = "";
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, []);

  const showToast = useCallback((message: string, action?: { label: string; run: () => void }) => {
    window.clearTimeout(toastTimer.current);
    setToast({ message, action });
    toastTimer.current = window.setTimeout(() => setToast(null), 6000);
  }, []);

  const trashPage = useCallback(
    async (id: string) => {
      const page = ws.pages.get(id) ?? client.getQueryData<{ team_id: string; title: string }>(["page", ws.org.id, id]);
      try {
        await api.deleteContext(ws.org.id, id);
      } catch (err) {
        showToast(`Could not delete: ${err instanceof Error ? err.message : String(err)}`);
        return;
      }
      client.removeQueries({ queryKey: ["page", ws.org.id, id] });
      ws.refreshPages(page?.team_id);
      void client.invalidateQueries({ queryKey: ["trash", ws.org.id] });
      void client.invalidateQueries({ queryKey: ["links", ws.org.id] });
      if (location === `/page/${id}`) navigate("/");
      showToast(`Moved “${page?.title ?? "page"}” to Trash`, {
        label: "Undo",
        run: () => {
          void api.restore(ws.org.id, id).then(
            () => {
              ws.refreshPages(page?.team_id);
              void client.invalidateQueries({ queryKey: ["trash", ws.org.id] });
              navigate(`/page/${id}`);
            },
            (err: unknown) => showToast(`Could not restore: ${err instanceof Error ? err.message : String(err)}`),
          );
        },
      });
    },
    [client, location, navigate, showToast, ws],
  );

  // The app renews Claude's tokens in the background; when that is no longer
  // possible, ask the user to connect again.
  useEffect(() => {
    const unlisten = listen<string[]>(CLAUDE_EXPIRED_EVENT, (event) => {
      const names = event.payload.map((t) => (t === "code" ? "Claude Code" : "Claude Desktop")).join(" and ");
      showToast(`${names} lost access to nanace`, { label: "Reconnect", run: () => setConnectOpen(true) });
    });
    return () => void unlisten.then((stop) => stop());
  }, [showToast]);

  const layout: Layout = {
    sidebarOpen: sidebar.open,
    openSidebar: () => setSidebar((s) => ({ ...s, open: true })),
    newPage,
    importMemory: () => setImportOpen(true),
    trashPage,
    openTrash: () => setTrashOpen(true),
    connectClaude: () => setConnectOpen(true),
    deleteAccount: () => setDeleteAccountOpen(true),
    toast: showToast,
  };

  return (
    <LayoutContext.Provider value={layout}>
      <div className="flex h-full">
        {sidebar.open && (
          <>
            <Sidebar
              width={sidebar.width}
              serverUrl={info.server_url}
              onCollapse={() => setSidebar((s) => ({ ...s, open: false }))}
              onSearch={() => setPalette(true)}
              onNewPage={(teamId) => void newPage(teamId)}
              onMembers={setMembersOf}
              onManageTeams={() => setTeamsOpen(true)}
              onSignOut={onSignOut}
            />
            <div
              className="w-px shrink-0 cursor-col-resize bg-line hover:w-[3px] hover:bg-accent/40"
              onMouseDown={() => {
                resizing.current = true;
                document.body.style.cursor = "col-resize";
              }}
            />
          </>
        )}
        <main className="min-w-0 flex-1 bg-surface">
          <Switch>
            <Route path="/" component={Home} />
            <Route path="/graph" component={GraphView} />
            <Route path="/page/:id">{(params) => <PageView key={params.id} id={params.id} />}</Route>
            <Route>
              <Home />
            </Route>
          </Switch>
        </main>
      </div>
      {palette && <CommandPalette onClose={() => setPalette(false)} />}
      {membersOf && <MembersDialog team={membersOf} onClose={() => setMembersOf(null)} />}
      {teamsOpen && <TeamsDialog onClose={() => setTeamsOpen(false)} />}
      {importOpen && <ImportMemoryDialog onClose={() => setImportOpen(false)} />}
      {trashOpen && <TrashDialog onClose={() => setTrashOpen(false)} />}
      {connectOpen && <ConnectClaudeDialog serverUrl={info.server_url} onClose={() => setConnectOpen(false)} />}
      {deleteAccountOpen && <DeleteAccountDialog onClose={() => setDeleteAccountOpen(false)} />}
      {toast && <Toast message={toast.message} action={toast.action} onClose={() => setToast(null)} />}
    </LayoutContext.Provider>
  );
}

function SignedIn({ info, onSignOut }: { info: SessionInfo; onSignOut: () => void }) {
  const me = useQuery({ queryKey: ["me"], queryFn: api.me, retry: 1 });
  if (me.isLoading) {
    return (
      <Center>
        <Spinner />
      </Center>
    );
  }
  if (!me.data) {
    return (
      <Center>
        <div className="text-center text-ink-2">
          <p className="mb-2">Cannot reach {info.server_url}.</p>
          <p className="text-sm text-ink-3">{me.error instanceof Error ? me.error.message : String(me.error)}</p>
          <button className="mt-4 text-sm text-accent" onClick={() => void me.refetch()}>
            Try again
          </button>
        </div>
      </Center>
    );
  }
  if (me.data.memberships.length === 0) {
    return <Onboarding onCreated={() => void me.refetch()} />;
  }
  return (
    <WorkspaceProvider me={me.data}>
      {(ws) =>
        ws ? (
          <Shell ws={ws} info={info} onSignOut={onSignOut} />
        ) : (
          <Center>
            <Spinner />
          </Center>
        )
      }
    </WorkspaceProvider>
  );
}

export function App() {
  const [info, setInfo] = useState<SessionInfo | null>(null);
  const client = useQueryClient();
  useExternalLinks();

  useEffect(() => {
    void session.info().then(setInfo);
    const onSignedOut = () => setInfo((i) => (i ? { ...i, signed_in: false } : i));
    window.addEventListener(SIGNED_OUT_EVENT, onSignedOut);
    return () => window.removeEventListener(SIGNED_OUT_EVENT, onSignedOut);
  }, []);

  if (!info) {
    return (
      <Center>
        <Spinner />
      </Center>
    );
  }
  if (!info.signed_in) {
    return (
      <SignIn
        info={info}
        onSignedIn={(next) => {
          client.clear();
          setInfo(next);
        }}
      />
    );
  }
  return (
    <SignedIn
      info={info}
      onSignOut={async () => {
        const next = await session.signOut();
        client.clear();
        setInfo(next);
      }}
    />
  );
}
