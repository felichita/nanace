import { Clock, FileText, Home as HomeIcon } from "lucide-react";
import { useLocation } from "wouter";

import { useWorkspace } from "../lib/workspace";
import { Crumb, Topbar } from "./Topbar";
import { relativeTime, Spinner, TeamIcon } from "./ui";

function greeting(): string {
  const h = new Date().getHours();
  if (h < 5) return "Good evening";
  if (h < 12) return "Good morning";
  if (h < 18) return "Good afternoon";
  return "Good evening";
}

export function Home() {
  const ws = useWorkspace();
  const [, navigate] = useLocation();
  const recent = [...ws.pages.values()].sort((a, b) => b.updated_at.localeCompare(a.updated_at)).slice(0, 12);
  const name = ws.me.email.split("@")[0];

  return (
    <div className="flex h-full flex-col">
      <Topbar
        left={
          <Crumb>
            <HomeIcon size={14} />
            Home
          </Crumb>
        }
      />
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-[900px] px-12 pt-16 pb-24">
          <h1 className="mb-12 text-center text-[32px] font-semibold">
            {greeting()}, {name}
          </h1>

          <div className="mb-3 flex items-center gap-1.5 text-xs font-medium text-ink-2">
            <Clock size={13} /> Recently edited
          </div>
          {ws.loadingPages && recent.length === 0 ? (
            <Spinner />
          ) : recent.length === 0 ? (
            <p className="text-ink-3">No pages yet. Create one from the sidebar.</p>
          ) : (
            <div className="grid grid-cols-[repeat(auto-fill,minmax(160px,1fr))] gap-4">
              {recent.map((p) => {
                const team = ws.team(p.team_id);
                return (
                  <div
                    key={p.id}
                    onClick={() => navigate(`/page/${p.id}`)}
                    className="flex h-[148px] flex-col overflow-hidden rounded-2xl bg-sidebar shadow-[0_0_0_1px_var(--border)] transition-shadow hover:shadow-pop"
                  >
                    <div className="relative h-[52px] bg-hover">
                      <FileText size={26} className="absolute -bottom-3 left-4 text-ink-2" strokeWidth={1.5} />
                    </div>
                    <div className="flex flex-1 flex-col justify-between p-4 pt-5">
                      <div className="line-clamp-2 text-sm font-medium">{p.title}</div>
                      <div className="flex items-center gap-1.5 text-xs text-ink-3">
                        {team && <TeamIcon id={team.id} name={team.name} size={14} />}
                        {relativeTime(p.updated_at)}
                      </div>
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
