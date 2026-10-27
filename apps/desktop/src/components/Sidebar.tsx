import {
  BrainCircuit,
  Check,
  ChevronDown,
  ChevronRight,
  ChevronsLeft,
  FileText,
  Home,
  LogOut,
  MoreHorizontal,
  Network,
  Plug,
  Plus,
  Search,
  Settings2,
  Trash2,
  Users,
} from "lucide-react";
import { useState } from "react";
import { useLocation } from "wouter";

import { canWrite, type Team } from "../lib/api";
import { useLayout } from "../lib/layout";
import { useWorkspace } from "../lib/workspace";
import { cx, IconButton, Menu, MenuItem, MenuSeparator, TeamIcon } from "./ui";

const COLLAPSED_KEY = "nanace.collapsedTeams";

function readCollapsed(): Set<string> {
  try {
    return new Set(JSON.parse(localStorage.getItem(COLLAPSED_KEY) ?? "[]") as string[]);
  } catch {
    return new Set();
  }
}

function Row({
  icon,
  label,
  active,
  onClick,
  shortcut,
  indent = 0,
  muted,
  children,
}: {
  icon: React.ReactNode;
  label: React.ReactNode;
  active?: boolean;
  onClick?: () => void;
  shortcut?: string;
  indent?: number;
  muted?: boolean;
  children?: React.ReactNode;
}) {
  return (
    <div
      onClick={onClick}
      className={cx(
        "group flex h-[30px] items-center gap-2 rounded-md pr-1.5 text-[14px]",
        active ? "bg-active font-medium text-ink" : "text-ink-2 hover:bg-hover",
        muted && "text-ink-3",
      )}
      style={{ paddingLeft: 8 + indent * 12 }}
    >
      <span className="flex w-5 shrink-0 justify-center">{icon}</span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {shortcut && <span className="text-xs text-ink-3 group-hover:hidden">{shortcut}</span>}
      {children && <span className="hidden items-center gap-0.5 group-hover:flex">{children}</span>}
    </div>
  );
}

function TeamSection({
  team,
  collapsed,
  onToggle,
  onNewPage,
  onMembers,
}: {
  team: Team;
  collapsed: boolean;
  onToggle: () => void;
  onNewPage: (teamId: string) => void;
  onMembers: (team: Team) => void;
}) {
  const ws = useWorkspace();
  const layout = useLayout();
  const [location, navigate] = useLocation();
  const pages = ws.pagesOf(team.id);
  const editable = canWrite(team.my_role);
  return (
    <div>
      <Row
        icon={
          <span className="relative">
            <span className="group-hover:invisible">
              <TeamIcon id={team.id} name={team.name} size={18} />
            </span>
            <span className="invisible absolute inset-0 flex items-center justify-center group-hover:visible">
              {collapsed ? <ChevronRight size={14} /> : <ChevronDown size={14} />}
            </span>
          </span>
        }
        label={team.name}
        onClick={onToggle}
      >
        <IconButton
          title="Members"
          onClick={(e) => {
            e.stopPropagation();
            onMembers(team);
          }}
        >
          <MoreHorizontal size={14} />
        </IconButton>
        {canWrite(team.my_role) && (
          <IconButton
            title="New page"
            onClick={(e) => {
              e.stopPropagation();
              onNewPage(team.id);
            }}
          >
            <Plus size={14} />
          </IconButton>
        )}
      </Row>
      {!collapsed && (
        <div>
          {pages.map((p) => (
            <Row
              key={p.id}
              indent={1}
              icon={<FileText size={15} className="text-ink-3" />}
              label={p.title}
              active={location === `/page/${p.id}`}
              onClick={() => navigate(`/page/${p.id}`)}
            >
              {editable && (
                <span onClick={(e) => e.stopPropagation()}>
                  <Menu
                    width={220}
                    trigger={({ toggle }) => (
                      <IconButton title="Delete, more…" onClick={toggle}>
                        <MoreHorizontal size={14} />
                      </IconButton>
                    )}
                  >
                    {(close) => (
                      <>
                        <MenuItem
                          icon={<Network size={14} />}
                          onClick={() => {
                            close();
                            navigate(`/graph?focus=${p.id}`);
                          }}
                        >
                          Show in graph
                        </MenuItem>
                        <MenuItem
                          danger
                          icon={<Trash2 size={14} />}
                          onClick={() => {
                            close();
                            void layout.trashPage(p.id);
                          }}
                        >
                          Move to Trash
                        </MenuItem>
                      </>
                    )}
                  </Menu>
                </span>
              )}
            </Row>
          ))}
          {pages.length === 0 && <div className="py-1 pl-[40px] text-[13px] text-ink-3">No pages inside</div>}
        </div>
      )}
    </div>
  );
}

export function Sidebar({
  width,
  onCollapse,
  onSearch,
  onNewPage,
  onMembers,
  onManageTeams,
  onSignOut,
  serverUrl,
}: {
  width: number;
  onCollapse: () => void;
  onSearch: () => void;
  onNewPage: (teamId?: string) => void;
  onMembers: (team: Team) => void;
  onManageTeams: () => void;
  onSignOut: () => void;
  serverUrl: string;
}) {
  const ws = useWorkspace();
  const layout = useLayout();
  const [location, navigate] = useLocation();
  const [collapsed, setCollapsed] = useState(readCollapsed);
  const toggle = (id: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      try {
        localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...next]));
      } catch {
        // Not persisted; fine.
      }
      return next;
    });
  const writable = ws.myTeams.some((t) => canWrite(t.my_role));

  return (
    <aside className="flex h-full shrink-0 flex-col bg-sidebar" style={{ width }}>
      {/* Room for the macOS traffic lights; drags the window. */}
      <div className="h-[38px] shrink-0" data-tauri-drag-region />
      <div className="px-2">
        <Menu
          width={280}
          trigger={({ toggle }) => (
            <div onClick={toggle} className="group flex h-[34px] items-center gap-2 rounded-md px-2 hover:bg-hover">
              <span className="flex h-[22px] w-[22px] items-center justify-center rounded bg-active text-[12px] font-semibold">
                {ws.org.name.slice(0, 1).toUpperCase()}
              </span>
              <span className="min-w-0 flex-1 truncate font-medium">{ws.org.name}</span>
              <ChevronDown size={14} className="text-ink-3" />
              <IconButton
                title="Collapse sidebar (⌘\)"
                className="invisible group-hover:visible"
                onClick={(e) => {
                  e.stopPropagation();
                  onCollapse();
                }}
              >
                <ChevronsLeft size={16} />
              </IconButton>
            </div>
          )}
        >
          {(close) => (
            <>
              <div className="truncate px-2 py-1 text-xs text-ink-3">{ws.me.email}</div>
              {ws.orgs.map((o) => (
                <MenuItem
                  key={o.id}
                  icon={<span className="text-xs font-semibold">{o.name.slice(0, 1).toUpperCase()}</span>}
                  right={o.id === ws.org.id ? <Check size={14} /> : undefined}
                  onClick={() => {
                    ws.setOrgId(o.id);
                    navigate("/");
                    close();
                  }}
                >
                  {o.name}
                </MenuItem>
              ))}
              <MenuSeparator />
              <MenuItem
                icon={<Settings2 size={14} />}
                onClick={() => {
                  onManageTeams();
                  close();
                }}
              >
                Teamspaces & members
              </MenuItem>
              <MenuItem
                icon={<Plug size={14} />}
                onClick={() => {
                  close();
                  layout.connectClaude();
                }}
              >
                Connect to Claude
              </MenuItem>
              {writable && (
                <MenuItem
                  icon={<BrainCircuit size={14} />}
                  onClick={() => {
                    close();
                    layout.importMemory();
                  }}
                >
                  Import Claude memory
                </MenuItem>
              )}
              <MenuItem
                icon={<LogOut size={14} />}
                onClick={() => {
                  close();
                  onSignOut();
                }}
              >
                Sign out
              </MenuItem>
              <div className="truncate px-2 pt-1 text-[11px] text-ink-3">{serverUrl}</div>
            </>
          )}
        </Menu>
      </div>

      <nav className="mt-1 space-y-px px-2">
        <Row icon={<Search size={16} />} label="Search" shortcut="⌘K" onClick={onSearch} />
        <Row icon={<Home size={16} />} label="Home" active={location === "/"} onClick={() => navigate("/")} />
        <Row
          icon={<Network size={16} />}
          label="Graph"
          active={location === "/graph"}
          onClick={() => navigate("/graph")}
        />
      </nav>

      <div className="mt-4 min-h-0 flex-1 overflow-y-auto px-2 pb-2">
        <div className="mb-1 flex items-center px-2 text-xs font-medium text-ink-3">Teamspaces</div>
        <div className="space-y-px">
          {ws.myTeams.map((team) => (
            <TeamSection
              key={team.id}
              team={team}
              collapsed={collapsed.has(team.id)}
              onToggle={() => toggle(team.id)}
              onNewPage={onNewPage}
              onMembers={onMembers}
            />
          ))}
          {ws.myTeams.length === 0 && (
            <div className="px-2 py-1 text-[13px] text-ink-3">
              You are not in any teamspace yet. Ask an admin to add you.
            </div>
          )}
        </div>
      </div>

      <div className="border-t border-line p-2">
        {writable && <Row icon={<Plus size={16} />} label="New page" onClick={() => onNewPage()} />}
        {writable && (
          <Row icon={<BrainCircuit size={16} />} label="Import Claude memory" onClick={layout.importMemory} />
        )}
        {ws.org.role === "admin" && (
          <Row icon={<Users size={16} />} label="Teamspaces & members" onClick={onManageTeams} />
        )}
        {writable && <Row icon={<Trash2 size={16} />} label="Trash" onClick={layout.openTrash} />}
        <Row icon={<Plug size={16} />} label="Connect to Claude" onClick={layout.connectClaude} />
      </div>
    </aside>
  );
}
