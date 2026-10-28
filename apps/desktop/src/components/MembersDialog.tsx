import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, Trash2, Users } from "lucide-react";
import { useState } from "react";

import { api, slugify, type OrgMember, type OrgRole, type Team, type TeamRole } from "../lib/api";
import { useWorkspace } from "../lib/workspace";
import { Button, Dialog, IconButton, Input, Spinner, TeamIcon } from "./ui";

const ROLES: { value: TeamRole; label: string }[] = [
  { value: "reader", label: "Can view" },
  { value: "writer", label: "Can edit" },
  { value: "admin", label: "Full access" },
];

function RoleSelect({ value, onChange, disabled }: { value: TeamRole; onChange: (r: TeamRole) => void; disabled?: boolean }) {
  return (
    <select
      value={value}
      disabled={disabled}
      onChange={(e) => onChange(e.target.value as TeamRole)}
      className="h-7 rounded-md bg-transparent px-1 text-[13px] text-ink-2 outline-none hover:bg-hover disabled:hover:bg-transparent"
    >
      {ROLES.map((r) => (
        <option key={r.value} value={r.value}>
          {r.label}
        </option>
      ))}
    </select>
  );
}

function TeamMembers({ team }: { team: Team }) {
  const ws = useWorkspace();
  const client = useQueryClient();
  const orgId = ws.org.id;
  const manage = ws.org.role === "admin" || team.my_role === "admin";
  const members = useQuery({ queryKey: ["members", orgId, team.id], queryFn: () => api.members(orgId, team.id) });
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<TeamRole>("writer");
  const [error, setError] = useState<string | null>(null);

  const refresh = () => {
    void client.invalidateQueries({ queryKey: ["members", orgId, team.id] });
    void client.invalidateQueries({ queryKey: ["teams", orgId] });
    void client.invalidateQueries({ queryKey: ["me"] });
  };
  const act = async (fn: () => Promise<unknown>) => {
    setError(null);
    try {
      await fn();
      refresh();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  };

  return (
    <div>
      {manage && (
        <form
          className="mb-3 flex gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            void act(async () => {
              await api.putMember(orgId, team.id, email, role);
              setEmail("");
            });
          }}
        >
          <Input placeholder="Email address" type="email" value={email} onChange={(e) => setEmail(e.target.value)} />
          <RoleSelect value={role} onChange={setRole} />
          <Button variant="primary" disabled={!email.includes("@")}>
            Invite
          </Button>
        </form>
      )}
      {error && <p className="mb-2 text-sm text-danger">{error}</p>}
      {members.isLoading && <Spinner />}
      <div className="space-y-px">
        {members.data?.map((m) => (
          <div key={m.user_id} className="flex h-10 items-center gap-3 rounded-md px-1 hover:bg-hover">
            <span className="flex h-7 w-7 items-center justify-center rounded-full bg-active text-xs font-medium">
              {(m.display_name ?? m.email).slice(0, 1).toUpperCase()}
            </span>
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm">{m.display_name ?? m.email}</div>
              {m.display_name && <div className="truncate text-xs text-ink-3">{m.email}</div>}
            </div>
            <RoleSelect
              value={m.role}
              disabled={!manage}
              onChange={(r) => void act(() => api.putMember(orgId, team.id, m.email, r))}
            />
            {manage && (
              <IconButton title="Remove from teamspace" onClick={() => void act(() => api.removeMember(orgId, team.id, m.user_id))}>
                <Trash2 size={14} />
              </IconButton>
            )}
          </div>
        ))}
      </div>
      {!manage && <p className="mt-3 text-xs text-ink-3">Only teamspace admins can change members.</p>}
    </div>
  );
}

/** Everyone in the workspace; org admins remove people from all teamspaces at once. */
function WorkspaceMembers() {
  const ws = useWorkspace();
  const client = useQueryClient();
  const orgId = ws.org.id;
  const members = useQuery({ queryKey: ["org-members", orgId], queryFn: () => api.orgMembers(orgId) });
  const [confirm, setConfirm] = useState<OrgMember | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function setRole(member: OrgMember, role: OrgRole) {
    setError(null);
    try {
      await api.setOrgRole(orgId, member.user_id, role);
      void client.invalidateQueries({ queryKey: ["org-members", orgId] });
      // Demoting yourself takes away this dialog's admin views.
      if (member.user_id === ws.me.user_id) void client.invalidateQueries({ queryKey: ["me"] });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function remove(member: OrgMember) {
    setError(null);
    try {
      await api.removeOrgMember(orgId, member.user_id);
      for (const key of ["org-members", "members", "teams"]) void client.invalidateQueries({ queryKey: [key, orgId] });
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setConfirm(null);
    }
  }

  return (
    <div>
      <p className="mb-3 text-xs text-ink-3">
        Removing someone from the workspace removes them from every teamspace; their access ends immediately.
      </p>
      {error && <p className="mb-2 text-sm text-danger">{error}</p>}
      {members.isLoading && <Spinner />}
      <div className="space-y-px">
        {members.data?.map((m) => (
          <div key={m.user_id} className="flex min-h-11 items-center gap-3 rounded-md px-1 py-1 hover:bg-hover">
            <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-active text-xs font-medium">
              {(m.display_name ?? m.email).slice(0, 1).toUpperCase()}
            </span>
            <div className="min-w-0 flex-1">
              <div className="flex items-center gap-2 truncate text-sm">
                {m.display_name ?? m.email}
                {m.user_id === ws.me.user_id && <span className="text-xs text-ink-3">(you)</span>}
              </div>
              <div className="truncate text-xs text-ink-3">
                {m.display_name ? `${m.email} · ` : ""}
                {m.teams.length ? m.teams.join(", ") : "No teamspaces"}
              </div>
            </div>
            <select
              value={m.role}
              onChange={(e) => void setRole(m, e.target.value as OrgRole)}
              title="Workspace admins manage teamspaces and members; they do not see pages of teamspaces they are not in."
              className="h-7 shrink-0 rounded-md bg-transparent px-1 text-xs text-ink-2 outline-none hover:bg-hover"
            >
              <option value="member">Member</option>
              <option value="admin">Workspace admin</option>
            </select>
            {m.user_id !== ws.me.user_id && (
              <IconButton title="Remove from workspace" onClick={() => setConfirm(m)}>
                <Trash2 size={14} />
              </IconButton>
            )}
          </div>
        ))}
      </div>
      {confirm && (
        <Dialog title="Remove from workspace?" onClose={() => setConfirm(null)} width={420}>
          <p className="mb-4 text-sm text-ink-2">
            {confirm.email} will lose access to {ws.org.name} and every teamspace in it
            {confirm.teams.length ? ` (${confirm.teams.join(", ")})` : ""}.
          </p>
          <div className="flex justify-end gap-2">
            <Button onClick={() => setConfirm(null)}>Cancel</Button>
            <Button variant="danger" onClick={() => void remove(confirm)}>
              Remove
            </Button>
          </div>
        </Dialog>
      )}
    </div>
  );
}

export function MembersDialog({ team, onClose }: { team: Team; onClose: () => void }) {
  return (
    <Dialog title={`${team.name} members`} onClose={onClose} width={520}>
      <TeamMembers team={team} />
    </Dialog>
  );
}

/** Org admins see every teamspace, including ones they are not in. */
export function TeamsDialog({ onClose }: { onClose: () => void }) {
  const ws = useWorkspace();
  const client = useQueryClient();
  const [selected, setSelected] = useState<Team | "workspace" | null>(
    ws.org.role === "admin" ? "workspace" : (ws.myTeams[0] ?? ws.teams[0] ?? null),
  );
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const isAdmin = ws.org.role === "admin";
  const teams = isAdmin ? ws.teams : ws.myTeams;

  return (
    <Dialog title="Teamspaces & members" onClose={onClose} width={760}>
      <div className="flex gap-4">
        <div className="w-[220px] shrink-0 space-y-px">
          {isAdmin && (
            <div
              onClick={() => setSelected("workspace")}
              className={`mb-2 flex h-8 items-center gap-2 rounded-md px-2 text-sm ${selected === "workspace" ? "bg-active" : "hover:bg-hover"}`}
            >
              <Users size={16} className="text-ink-2" />
              Workspace members
            </div>
          )}
          {teams.map((t) => (
            <div
              key={t.id}
              onClick={() => setSelected(t)}
              className={`flex h-8 items-center gap-2 rounded-md px-2 text-sm ${selected !== "workspace" && selected?.id === t.id ? "bg-active" : "hover:bg-hover"}`}
            >
              <TeamIcon id={t.id} name={t.name} size={18} />
              <span className="truncate">{t.name}</span>
              {!t.my_role && <span className="ml-auto text-[11px] text-ink-3">not a member</span>}
            </div>
          ))}
          {isAdmin && (
            <form
              className="mt-3 space-y-2 border-t border-line pt-3"
              onSubmit={async (e) => {
                e.preventDefault();
                setError(null);
                try {
                  const team = await api.createTeam(ws.org.id, slugify(name), name.trim());
                  setName("");
                  setSelected(team);
                  void client.invalidateQueries({ queryKey: ["teams", ws.org.id] });
                  void client.invalidateQueries({ queryKey: ["me"] });
                } catch (err) {
                  setError(err instanceof Error ? err.message : String(err));
                }
              }}
            >
              <Input placeholder="New teamspace" value={name} onChange={(e) => setName(e.target.value)} />
              <Button className="w-full" disabled={!name.trim()}>
                <Plus size={14} /> Create teamspace
              </Button>
              {error && <p className="text-xs text-danger">{error}</p>}
            </form>
          )}
        </div>
        <div className="min-w-0 flex-1">
          {selected === "workspace" ? (
            <WorkspaceMembers />
          ) : (
            selected && <TeamMembers key={selected.id} team={selected} />
          )}
        </div>
      </div>
    </Dialog>
  );
}
