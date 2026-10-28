import { useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowUpRight, Eye, FileText, Link2, MoreHorizontal, Network, Sparkles, Tag, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useLocation } from "wouter";

import { blocksToMarkdown, PageLinkContext, type Editor, type PageLinkResolver } from "../editor/schema";
import { PageEditor } from "../editor/PageEditor";
import { api, ApiError, canWrite, type Context, type LinkedContext } from "../lib/api";
import { useLayout } from "../lib/layout";
import { useWorkspace } from "../lib/workspace";
import { Crumb, Topbar } from "./Topbar";
import { Button, cx, IconButton, Menu, MenuItem, relativeTime, Spinner, TeamIcon } from "./ui";

const SAVE_DELAY_MS = 700;

type SaveState =
  | { kind: "idle" }
  | { kind: "saving" }
  | { kind: "saved"; at: string }
  | { kind: "conflict" }
  | { kind: "error"; message: string };

interface Saved {
  title: string;
  content: string;
  tags: string[];
  updatedAt: string;
}

function sameTags(a: string[], b: string[]) {
  return a.length === b.length && a.every((t, i) => t === b[i]);
}

function TitleInput({ value, onChange, readOnly, onEnter }: {
  value: string;
  onChange: (v: string) => void;
  readOnly: boolean;
  onEnter: () => void;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "0px";
    el.style.height = `${el.scrollHeight}px`;
  }, [value]);
  return (
    <textarea
      ref={ref}
      rows={1}
      value={value}
      readOnly={readOnly}
      placeholder="Untitled"
      maxLength={200}
      onChange={(e) => onChange(e.target.value.replace(/\n/g, ""))}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          onEnter();
        }
      }}
      className="w-full resize-none overflow-hidden bg-transparent px-[54px] text-[40px] leading-[1.2] font-bold outline-none placeholder:text-ink-3/60"
    />
  );
}

function Tags({ tags, onChange, readOnly }: { tags: string[]; onChange: (t: string[]) => void; readOnly: boolean }) {
  const [draft, setDraft] = useState("");
  const add = () => {
    const t = draft.trim().toLowerCase();
    if (t && !tags.includes(t) && tags.length < 20) onChange([...tags, t].sort());
    setDraft("");
  };
  if (readOnly && tags.length === 0) return null;
  return (
    <div className="flex min-h-[34px] items-center gap-2 px-[54px] text-sm">
      <span className="flex w-[120px] shrink-0 items-center gap-1.5 text-ink-2">
        <Tag size={14} /> Tags
      </span>
      <div className="flex flex-wrap items-center gap-1">
        {tags.map((t) => (
          <span key={t} className="inline-flex items-center gap-1 rounded bg-chip px-1.5 py-0.5 text-[13px]">
            {t}
            {!readOnly && (
              <X size={12} className="text-ink-3 hover:text-ink" onClick={() => onChange(tags.filter((x) => x !== t))} />
            )}
          </span>
        ))}
        {!readOnly && (
          <input
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === ",") {
                e.preventDefault();
                add();
              }
            }}
            onBlur={add}
            placeholder={tags.length ? "" : "Add a tag"}
            className="w-28 bg-transparent text-[13px] outline-none placeholder:text-ink-3"
          />
        )}
      </div>
    </div>
  );
}

function ConnectionList({
  icon,
  title,
  items,
  empty,
  score,
}: {
  icon: React.ReactNode;
  title: string;
  items: (LinkedContext & { score?: number })[];
  empty: string;
  score?: boolean;
}) {
  const [, navigate] = useLocation();
  const ws = useWorkspace();
  return (
    <div>
      <div className="mb-1 flex items-center gap-1.5 text-xs font-medium text-ink-3">
        {icon}
        {title}
        <span className="text-ink-3/70">{items.length || ""}</span>
      </div>
      {items.length === 0 && <div className="py-1 text-[13px] text-ink-3">{empty}</div>}
      {items.map((c) => (
        <div
          key={c.id}
          onClick={() => navigate(`/page/${c.id}`)}
          className="group flex h-[28px] items-center gap-2 rounded-md px-1.5 text-sm hover:bg-hover"
        >
          <FileText size={14} className="shrink-0 text-ink-3" />
          <span className="truncate">{c.title}</span>
          <span className="truncate text-xs text-ink-3">{ws.team(c.team_id)?.name}</span>
          {score && c.score !== undefined && (
            <span className="ml-auto text-xs text-ink-3">{Math.round(c.score * 100)}%</span>
          )}
        </div>
      ))}
    </div>
  );
}

export function PageView({ id }: { id: string }) {
  const ws = useWorkspace();
  const layout = useLayout();
  const client = useQueryClient();
  const [, navigate] = useLocation();
  const orgId = ws.org.id;

  const pageQuery = useQuery({
    queryKey: ["page", orgId, id],
    queryFn: () => api.context(orgId, id),
    staleTime: Infinity,
    refetchOnWindowFocus: false,
    retry: (count, err) => !(err instanceof ApiError && err.status < 500) && count < 2,
  });
  const linksQuery = useQuery({
    queryKey: ["links", orgId, id],
    queryFn: () => api.links(orgId, id),
  });

  const page = pageQuery.data;
  const team = page ? ws.team(page.team_id) : undefined;
  const editable = canWrite(team?.my_role);

  const [title, setTitle] = useState("");
  const [tags, setTags] = useState<string[]>([]);
  const [editorKey, setEditorKey] = useState(0);
  const [save, setSave] = useState<SaveState>({ kind: "idle" });

  const editorRef = useRef<Editor | null>(null);
  const saved = useRef<Saved | null>(null);
  const titleRef = useRef(title);
  const tagsRef = useRef(tags);
  titleRef.current = title;
  tagsRef.current = tags;
  const timer = useRef<number | undefined>(undefined);
  const inFlight = useRef<Promise<void> | null>(null);
  const blocked = useRef(false);

  // (Re)load local state whenever a fresh copy of the page arrives.
  const loaded = useRef<string | null>(null);
  useEffect(() => {
    if (!page || loaded.current === `${page.id}:${editorKey}`) return;
    loaded.current = `${page.id}:${editorKey}`;
    saved.current = { title: page.title, content: page.content, tags: page.tags, updatedAt: page.updated_at };
    // Update the refs now, not on the next render: a flush before then (StrictMode
    // runs the unmount flush right after this effect) would save the empty
    // initial title and tags.
    titleRef.current = page.title;
    tagsRef.current = page.tags;
    blocked.current = false;
    setTitle(page.title);
    setTags(page.tags);
    setSave({ kind: "idle" });
  }, [page, editorKey]);

  const flush = useCallback(async () => {
    window.clearTimeout(timer.current);
    if (inFlight.current) await inFlight.current;
    const base = saved.current;
    const editor = editorRef.current;
    if (!base || !editor || blocked.current || !editable) return;
    const next = {
      title: titleRef.current.trim() || "Untitled",
      content: blocksToMarkdown(editor),
      tags: tagsRef.current,
    };
    if (next.title === base.title && next.content === base.content && sameTags(next.tags, base.tags)) return;

    setSave({ kind: "saving" });
    const run = (async () => {
      try {
        const updated = await api.updateContext(orgId, id, { ...next, base_updated_at: base.updatedAt });
        saved.current = { ...next, tags: updated.tags, updatedAt: updated.updated_at };
        client.setQueryData<Context>(["page", orgId, id], updated);
        setSave({ kind: "saved", at: updated.updated_at });
        if (next.title !== base.title) ws.refreshPages(updated.team_id);
        void client.invalidateQueries({ queryKey: ["links", orgId] });
        void client.invalidateQueries({ queryKey: ["graph", orgId] });
      } catch (err) {
        blocked.current = true;
        if (err instanceof ApiError && err.status === 409) setSave({ kind: "conflict" });
        else setSave({ kind: "error", message: err instanceof Error ? err.message : String(err) });
      }
    })();
    inFlight.current = run;
    await run;
    inFlight.current = null;
  }, [client, editable, id, orgId, ws]);

  const schedule = useCallback(() => {
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => void flush(), SAVE_DELAY_MS);
  }, [flush]);

  // Save pending edits when leaving the page.
  const flushRef = useRef(flush);
  flushRef.current = flush;
  useEffect(() => () => void flushRef.current(), [id]);

  const reload = async () => {
    await client.invalidateQueries({ queryKey: ["page", orgId, id] });
    await pageQuery.refetch();
    setEditorKey((k) => k + 1);
  };

  const resolver: PageLinkResolver = {
    resolve: (t) => ws.pageByTitle(t),
    open: (t) => {
      const target = ws.pageByTitle(t);
      if (target) navigate(`/page/${target}`);
      else if (editable && page) void layout.newPage(page.team_id, t);
    },
  };

  const onReady = useCallback((editor: Editor) => {
    editorRef.current = editor;
  }, []);

  if (pageQuery.isLoading) {
    return (
      <div className="flex h-full flex-col">
        <Topbar left={null} />
        <div className="flex flex-1 items-center justify-center">
          <Spinner />
        </div>
      </div>
    );
  }
  if (!page) {
    return (
      <div className="flex h-full flex-col">
        <Topbar left={null} />
        <div className="flex flex-1 flex-col items-center justify-center gap-3 text-ink-2">
          <p>This page does not exist or you do not have access to it.</p>
          <Button onClick={() => navigate("/")}>Go home</Button>
        </div>
      </div>
    );
  }

  const status =
    save.kind === "saving"
      ? "Saving…"
      : save.kind === "saved"
        ? `Edited ${relativeTime(save.at)}`
        : save.kind === "error"
          ? "Not saved"
          : `Edited ${relativeTime(page.updated_at)}`;

  return (
    <div className="flex h-full flex-col">
      <Topbar
        left={
          <>
            {team && (
              <Crumb>
                <TeamIcon id={team.id} name={team.name} size={16} />
                {team.name}
              </Crumb>
            )}
            <span className="text-ink-3">/</span>
            <Crumb>
              <FileText size={14} className="shrink-0 text-ink-2" />
              <span className="truncate">{title || "Untitled"}</span>
            </Crumb>
          </>
        }
        right={
          <>
            <span className={cx("px-2 text-ink-3", save.kind === "error" && "text-danger")}>{status}</span>
            {!editable && (
              <span className="flex items-center gap-1 rounded bg-chip px-2 py-0.5 text-xs text-ink-2">
                <Eye size={12} /> Can view
              </span>
            )}
            <IconButton title="Show in graph" onClick={() => navigate(`/graph?focus=${id}`)} className="h-7 w-7">
              <Network size={16} />
            </IconButton>
            {editable && (
              <Menu
                align="right"
                width={200}
                trigger={({ toggle }) => (
                  <IconButton onClick={toggle} className="h-7 w-7">
                    <MoreHorizontal size={16} />
                  </IconButton>
                )}
              >
                {(close) => (
                  <MenuItem
                    danger
                    icon={<Trash2 size={14} />}
                    onClick={() => {
                      close();
                      blocked.current = true;
                      window.clearTimeout(timer.current);
                      void layout.trashPage(id);
                    }}
                  >
                    Move to Trash
                  </MenuItem>
                )}
              </Menu>
            )}
          </>
        }
      />

      {(save.kind === "conflict" || save.kind === "error") && (
        <div className="mx-auto mt-1 flex w-full max-w-[816px] items-center gap-3 rounded-md bg-danger/10 px-4 py-2 text-sm">
          <span className="flex-1">
            {save.kind === "conflict"
              ? "Someone else changed this page. Your latest edits were not saved."
              : `Your latest edits were not saved: ${save.message}`}
          </span>
          {save.kind === "error" && (
            <Button
              onClick={() => {
                blocked.current = false;
                void flush();
              }}
            >
              Retry
            </Button>
          )}
          <Button onClick={() => void reload()}>Reload page</Button>
        </div>
      )}

      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="mx-auto max-w-[816px] pt-[72px] pb-[30vh]">
          <TitleInput
            value={title}
            readOnly={!editable}
            onChange={(v) => {
              setTitle(v);
              schedule();
            }}
            onEnter={() => editorRef.current?.focus()}
          />
          <div className="mt-2 mb-3">
            <Tags
              tags={tags}
              readOnly={!editable}
              onChange={(t) => {
                setTags(t);
                tagsRef.current = t;
                schedule();
              }}
            />
          </div>
          <PageLinkContext.Provider value={resolver}>
            <PageEditor
              key={`${id}:${editorKey}`}
              markdown={page.content}
              editable={editable}
              onChange={schedule}
              onReady={onReady}
            />
          </PageLinkContext.Provider>

          <section className="mx-[54px] mt-16 grid grid-cols-1 gap-6 border-t border-line pt-6 md:grid-cols-3">
            <ConnectionList
              icon={<ArrowUpRight size={12} />}
              title="Backlinks"
              items={linksQuery.data?.backlinks ?? []}
              empty="No pages link here."
            />
            <ConnectionList
              icon={<Link2 size={12} />}
              title="Links"
              items={linksQuery.data?.outgoing ?? []}
              empty="Type @ to link a page."
            />
            <ConnectionList
              icon={<Sparkles size={12} />}
              title="Related"
              items={linksQuery.data?.related ?? []}
              empty="Nothing similar yet."
              score
            />
          </section>
        </div>
      </div>

    </div>
  );
}
