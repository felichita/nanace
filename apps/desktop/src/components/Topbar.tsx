import { ChevronsRight } from "lucide-react";
import type { ReactNode } from "react";

import { useLayout } from "../lib/layout";
import { IconButton } from "./ui";

/** Page header: breadcrumbs left, actions right. Doubles as the window drag area. */
export function Topbar({ left, right }: { left: ReactNode; right?: ReactNode }) {
  const layout = useLayout();
  return (
    <header
      className="flex h-[44px] shrink-0 items-center gap-2 px-3"
      style={{ paddingLeft: layout.sidebarOpen ? 12 : 84 }}
      data-tauri-drag-region
    >
      {!layout.sidebarOpen && (
        <IconButton title="Open sidebar (⌘\)" onClick={layout.openSidebar}>
          <ChevronsRight size={16} />
        </IconButton>
      )}
      <div className="flex min-w-0 flex-1 items-center gap-1 text-sm" data-tauri-drag-region>
        {left}
      </div>
      {right && <div className="flex shrink-0 items-center gap-1 text-sm">{right}</div>}
    </header>
  );
}

export function Crumb({ children, onClick }: { children: ReactNode; onClick?: () => void }) {
  return (
    <span
      onClick={onClick}
      className="flex min-w-0 items-center gap-1.5 truncate rounded px-1.5 py-0.5 text-ink hover:bg-hover"
    >
      {children}
    </span>
  );
}
