import { useQuery } from "@tanstack/react-query";
import { forceX, forceY } from "d3-force";
import { Network } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import ForceGraph2D, { type ForceGraphMethods, type LinkObject, type NodeObject } from "react-force-graph-2d";
import { useLocation, useSearch } from "wouter";

import { api, type GraphEdge, type GraphNode } from "../lib/api";
import { useColorScheme } from "../lib/layout";
import { useWorkspace } from "../lib/workspace";
import { Crumb, Topbar } from "./Topbar";
import { cx, Spinner, teamColor } from "./ui";

type Node = NodeObject<GraphNode & { degree: number }>;
type Link = LinkObject<Node, GraphEdge>;

/** Link ends are ids until the simulation replaces them with node objects. */
const endId = (end: unknown) => (typeof end === "object" && end !== null ? String((end as Node).id) : String(end));

function useSize(ref: React.RefObject<HTMLDivElement | null>) {
  const [size, setSize] = useState({ width: 0, height: 0 });
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([entry]) =>
      setSize({ width: entry.contentRect.width, height: entry.contentRect.height }),
    );
    ro.observe(el);
    return () => ro.disconnect();
  }, [ref]);
  return size;
}

function Toggle({ on, onClick, children }: { on: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      onClick={onClick}
      className={cx(
        "flex h-7 items-center gap-2 rounded-md px-2 text-[13px]",
        on ? "text-ink" : "text-ink-3",
        "hover:bg-hover",
      )}
    >
      <span
        className={cx(
          "relative h-3.5 w-6 rounded-full transition-colors",
          on ? "bg-accent" : "bg-active",
        )}
      >
        <span
          className={cx(
            "absolute top-0.5 h-2.5 w-2.5 rounded-full bg-white transition-all",
            on ? "left-3" : "left-0.5",
          )}
        />
      </span>
      {children}
    </button>
  );
}

export function GraphView() {
  const ws = useWorkspace();
  const scheme = useColorScheme();
  const [, navigate] = useLocation();
  const focus = new URLSearchParams(useSearch()).get("focus");
  const [teamId, setTeamId] = useState<string>("");
  const [showLinks, setShowLinks] = useState(true);
  const [showSimilar, setShowSimilar] = useState(true);
  const [minScore, setMinScore] = useState(0.7);
  const [debouncedScore, setDebouncedScore] = useState(minScore);
  const [hover, setHover] = useState<string | null>(focus);
  const container = useRef<HTMLDivElement>(null);
  const size = useSize(container);
  const fg = useRef<ForceGraphMethods<Node, Link> | undefined>(undefined);
  const fitted = useRef(false);

  useEffect(() => {
    const t = window.setTimeout(() => setDebouncedScore(minScore), 300);
    return () => window.clearTimeout(t);
  }, [minScore]);

  const graphQuery = useQuery({
    queryKey: ["graph", ws.org.id, teamId, debouncedScore],
    queryFn: () => api.graph(ws.org.id, { team_id: teamId || undefined, min_score: debouncedScore }),
    placeholderData: (prev) => prev,
  });

  // Force-graph mutates its input, so build fresh objects per response.
  const data = useMemo(() => {
    const g = graphQuery.data;
    if (!g) return { nodes: [] as Node[], links: [] as Link[] };
    const edges = g.edges.filter((e) => (e.kind === "link" ? showLinks : showSimilar));
    const degree = new Map<string, number>();
    for (const e of edges) {
      degree.set(e.source, (degree.get(e.source) ?? 0) + 1);
      degree.set(e.target, (degree.get(e.target) ?? 0) + 1);
    }
    return {
      nodes: g.nodes.map((n) => ({ ...n, degree: degree.get(n.id) ?? 0 })) as Node[],
      links: edges.map((e) => ({ ...e })) as Link[],
    };
  }, [graphQuery.data, showLinks, showSimilar]);
  useEffect(() => {
    fitted.current = false;
    // Spread nodes out: the default charge packs small graphs into a clump.
    fg.current?.d3Force("charge")?.strength(-160).distanceMax(260);
    fg.current?.d3Force("link")?.distance(50);
    // Gentle pull to the center keeps unlinked pages on screen.
    fg.current?.d3Force("x", forceX(0).strength(0.07));
    fg.current?.d3Force("y", forceY(0).strength(0.07));
    fg.current?.d3ReheatSimulation();
  }, [data]);

  const neighbors = useMemo(() => {
    const map = new Map<string, Set<string>>();
    for (const l of data.links) {
      const s = endId(l.source);
      const t = endId(l.target);
      if (!map.has(s)) map.set(s, new Set());
      if (!map.has(t)) map.set(t, new Set());
      map.get(s)!.add(t);
      map.get(t)!.add(s);
    }
    return map;
  }, [data]);

  useEffect(() => {
    if (!focus || !fg.current) return;
    const t = window.setTimeout(() => {
      const node = data.nodes.find((n) => n.id === focus);
      if (node?.x !== undefined && node.y !== undefined) {
        fg.current?.centerAt(node.x, node.y, 600);
        fg.current?.zoom(2.5, 600);
      }
    }, 900);
    return () => window.clearTimeout(t);
  }, [focus, data]);

  const dark = scheme === "dark";
  const ink = dark ? "rgba(255,255,255,0.81)" : "rgb(55,53,47)";
  const faint = dark ? "rgba(255,255,255,0.12)" : "rgba(55,53,47,0.12)";
  const linkColor = dark ? "rgba(255,255,255,0.35)" : "rgba(55,53,47,0.35)";
  const similarColor = dark ? "rgba(35,131,226,0.45)" : "rgba(35,131,226,0.4)";
  const active = (id: string) => !hover || id === hover || neighbors.get(hover)?.has(id);

  const teamsInGraph = ws.myTeams;

  return (
    <div className="flex h-full flex-col">
      <Topbar
        left={
          <Crumb>
            <Network size={14} />
            Graph
          </Crumb>
        }
        right={
          <span className="px-2 text-ink-3">
            {graphQuery.isFetching ? (
              <Spinner className="h-3 w-3" />
            ) : graphQuery.data ? (
              `${data.nodes.length} pages · ${data.links.length} connections${graphQuery.data.truncated ? " (most recent 2000)" : ""}`
            ) : null}
          </span>
        }
      />
      <div ref={container} className="relative min-h-0 flex-1">
        {size.width > 0 && (
          <ForceGraph2D<Node, Link>
            ref={fg}
            width={size.width}
            height={size.height}
            graphData={data}
            backgroundColor="rgba(0,0,0,0)"
            nodeId="id"
            nodeRelSize={4}
            nodeVal={(n) => 1 + Math.sqrt(n.degree) * 1.5}
            nodeLabel={() => ""}
            warmupTicks={80}
            cooldownTicks={200}
            onEngineStop={() => {
              // Fit once per load; later reheats (dragging) keep the user's view.
              if (!fitted.current && !focus) fg.current?.zoomToFit(400, 80);
              fitted.current = true;
            }}
            linkColor={(l) => {
              const on = !hover || endId(l.source) === hover || endId(l.target) === hover;
              if (!on) return faint;
              return l.kind === "link" ? linkColor : similarColor;
            }}
            linkWidth={(l) => (l.kind === "link" ? 1.2 : 0.6 + (l.score ?? 0) * 1.2)}
            linkLineDash={(l) => (l.kind === "semantic" ? [3, 3] : null)}
            linkDirectionalArrowLength={(l) => (l.kind === "link" ? 3 : 0)}
            linkDirectionalArrowRelPos={0.92}
            onNodeHover={(n) => setHover(n ? String(n.id) : focus)}
            onNodeClick={(n) => navigate(`/page/${n.id}`)}
            onBackgroundClick={() => setHover(null)}
            nodeCanvasObjectMode={() => "replace"}
            nodeCanvasObject={(node, ctx, scale) => {
              const id = String(node.id);
              const r = 2.5 + Math.sqrt(node.degree) * 1.6;
              const on = active(id);
              ctx.globalAlpha = on ? 1 : 0.18;
              ctx.beginPath();
              ctx.arc(node.x ?? 0, node.y ?? 0, r, 0, 2 * Math.PI);
              ctx.fillStyle = teamColor(node.team_id);
              ctx.fill();
              if (id === hover || id === focus) {
                ctx.lineWidth = 1.5 / scale;
                ctx.strokeStyle = ink;
                ctx.stroke();
              }
              const showLabel = scale > 1.6 || id === hover || (on && hover !== null) || node.degree >= 4;
              if (showLabel) {
                const fontSize = Math.max(11 / scale, 2);
                ctx.font = `${id === hover ? 600 : 400} ${fontSize}px -apple-system, system-ui, sans-serif`;
                ctx.textAlign = "center";
                ctx.textBaseline = "top";
                ctx.fillStyle = ink;
                const label = node.title.length > 40 ? `${node.title.slice(0, 38)}…` : node.title;
                ctx.fillText(label, node.x ?? 0, (node.y ?? 0) + r + 2 / scale);
              }
              ctx.globalAlpha = 1;
            }}
            nodePointerAreaPaint={(node, color, ctx) => {
              const r = 4 + Math.sqrt(node.degree) * 1.6;
              ctx.fillStyle = color;
              ctx.beginPath();
              ctx.arc(node.x ?? 0, node.y ?? 0, r, 0, 2 * Math.PI);
              ctx.fill();
            }}
          />
        )}

        {graphQuery.data && data.nodes.length === 0 && (
          <div className="absolute inset-0 flex items-center justify-center text-ink-3">
            No pages yet. Create a page and link others with @.
          </div>
        )}

        <div className="absolute top-3 right-3 w-[220px] space-y-1 rounded-lg bg-popover p-2 shadow-pop">
          <select
            value={teamId}
            onChange={(e) => setTeamId(e.target.value)}
            className="mb-1 h-7 w-full rounded-md bg-hover px-2 text-[13px] outline-none"
          >
            <option value="">All teamspaces</option>
            {teamsInGraph.map((t) => (
              <option key={t.id} value={t.id}>
                {t.name}
              </option>
            ))}
          </select>
          <Toggle on={showLinks} onClick={() => setShowLinks((v) => !v)}>
            <span className="h-px w-4 bg-ink-2" /> Links
          </Toggle>
          <Toggle on={showSimilar} onClick={() => setShowSimilar((v) => !v)}>
            <span className="w-4 border-t border-dashed border-accent" /> Similar
          </Toggle>
          {showSimilar && (
            <label className="block px-2 pt-1 text-[12px] text-ink-3">
              Similarity ≥ {Math.round(minScore * 100)}%
              <input
                type="range"
                min={0.5}
                max={0.95}
                step={0.01}
                value={minScore}
                onChange={(e) => setMinScore(Number(e.target.value))}
                className="mt-1 w-full accent-[var(--accent)]"
              />
            </label>
          )}
          <div className="space-y-0.5 border-t border-line px-2 pt-2">
            {teamsInGraph.map((t) => (
              <div key={t.id} className="flex items-center gap-2 text-[12px] text-ink-2">
                <span className="h-2.5 w-2.5 rounded-full" style={{ background: teamColor(t.id) }} />
                <span className="truncate">{t.name}</span>
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
