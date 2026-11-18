import { useSyncExternalStore, createContext, useContext } from "react";

export interface Layout {
  sidebarOpen: boolean;
  openSidebar(): void;
  newPage(teamId?: string, title?: string): Promise<string | null>;
  importMemory(): void;
  /** Moves a page to the Trash and offers Undo. */
  trashPage(id: string): Promise<void>;
  openTrash(): void;
  connectClaude(): void;
  /** Opens the agent-to-agent (P2P) settings and activity. */
  openP2P(): void;
  deleteAccount(): void;
  toast(message: string, action?: { label: string; run: () => void }): void;
}

export const LayoutContext = createContext<Layout>({
  sidebarOpen: true,
  openSidebar: () => {},
  newPage: async () => null,
  importMemory: () => {},
  trashPage: async () => {},
  openTrash: () => {},
  connectClaude: () => {},
  openP2P: () => {},
  deleteAccount: () => {},
  toast: () => {},
});

export const useLayout = () => useContext(LayoutContext);

const darkQuery = window.matchMedia("(prefers-color-scheme: dark)");

export function useColorScheme(): "light" | "dark" {
  return useSyncExternalStore(
    (cb) => {
      darkQuery.addEventListener("change", cb);
      return () => darkQuery.removeEventListener("change", cb);
    },
    () => (darkQuery.matches ? "dark" : "light"),
  );
}
