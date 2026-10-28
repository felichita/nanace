// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, StrictMode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { api, type Context } from "../lib/api";
import { PageView } from "./PageView";

const PAGE: Context = {
  id: "page-1",
  org_id: "org-1",
  team_id: "team-1",
  title: "Deploy Guide",
  content: "Body",
  tags: ["ops"],
  created_by: "user-1",
  created_at: "2026-10-01T00:00:00Z",
  updated_at: "2026-10-02T00:00:00Z",
} as Context;

const { workspace } = vi.hoisted(() => {
  // jsdom has no matchMedia; lib/layout reads it at import time.
  window.matchMedia = () => ({ matches: false, addEventListener() {}, removeEventListener() {} }) as never;
  const team = { id: "team-1", slug: "ops", name: "Ops", my_role: "writer" };
  return {
    workspace: {
      org: { id: "org-1" },
      team: (id: string) => (id === team.id ? team : undefined),
      pageByTitle: () => undefined,
      refreshPages: () => {},
    },
  };
});

vi.mock("../lib/workspace", () => ({ useWorkspace: () => workspace }));
vi.mock("../lib/api", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../lib/api")>()),
  api: { context: vi.fn(), links: vi.fn(), updateContext: vi.fn() },
}));
// The real editor needs a browser layout; the stub hands PageView an editor whose
// markdown matches the loaded page, so only the title and tags can differ.
vi.mock("../editor/schema", async () => ({
  blocksToMarkdown: () => "Body",
  PageLinkContext: (await import("react")).createContext(null),
}));
vi.mock("../editor/PageEditor", async () => {
  const { useEffect } = await import("react");
  return {
    PageEditor: ({ onReady }: { onReady: (editor: unknown) => void }) => {
      useEffect(() => onReady({ focus: () => {} }), [onReady]);
      return null;
    },
  };
});

describe("PageView", () => {
  let root: Root;
  let client: QueryClient;

  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    vi.mocked(api.links).mockResolvedValue({ outgoing: [], backlinks: [], related: [] });
    vi.mocked(api.updateContext).mockImplementation(async (_org, _id, input) => ({
      ...PAGE,
      ...input,
      updated_at: "2026-10-03T00:00:00Z",
    }));
    client = new QueryClient();
    // A page opened before is already cached, so the first render has its data.
    client.setQueryData(["page", "org-1", PAGE.id], PAGE);
    root = createRoot(document.body.appendChild(document.createElement("div")));
  });

  afterEach(() => {
    act(() => root.unmount());
    document.body.innerHTML = "";
    vi.clearAllMocks();
  });

  const render = async () => {
    await act(async () =>
      root.render(
        <StrictMode>
          <QueryClientProvider client={client}>
            <PageView id={PAGE.id} />
          </QueryClientProvider>
        </StrictMode>,
      ),
    );
  };

  it("does not save when a cached page is opened", async () => {
    await render();
    expect(document.querySelector("textarea")?.value).toBe("Deploy Guide");

    await act(async () => root.unmount());
    expect(api.updateContext).not.toHaveBeenCalled();
  });

  it("saves an edited title when leaving the page", async () => {
    await render();
    const input = document.querySelector("textarea")!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(input, "Deploy Runbook");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });

    await act(async () => root.unmount());
    expect(api.updateContext).toHaveBeenCalledTimes(1);
    expect(vi.mocked(api.updateContext).mock.calls[0][2]).toMatchObject({ title: "Deploy Runbook", tags: ["ops"] });
  });
});
