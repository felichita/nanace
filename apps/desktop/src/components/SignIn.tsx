import { ChevronDown, ChevronRight } from "lucide-react";
import { useState } from "react";

import { api, session, slugify, type SessionInfo } from "../lib/api";
import { Button, Input, Spinner } from "./ui";

function GoogleMark() {
  return (
    <svg width="16" height="16" viewBox="0 0 48 48" aria-hidden>
      <path fill="#FFC107" d="M43.6 20.5H42V20H24v8h11.3C33.7 32.7 29.2 36 24 36c-6.6 0-12-5.4-12-12s5.4-12 12-12c3.1 0 5.8 1.2 7.9 3.1l5.7-5.7C34 6.1 29.3 4 24 4 12.9 4 4 12.9 4 24s8.9 20 20 20 20-8.9 20-20c0-1.3-.1-2.4-.4-3.5z" />
      <path fill="#FF3D00" d="m6.3 14.7 6.6 4.8C14.7 15.1 19 12 24 12c3.1 0 5.8 1.2 7.9 3.1l5.7-5.7C34 6.1 29.3 4 24 4 16.3 4 9.7 8.3 6.3 14.7z" />
      <path fill="#4CAF50" d="M24 44c5.2 0 9.9-2 13.4-5.2l-6.2-5.2C29.2 35.1 26.7 36 24 36c-5.2 0-9.6-3.3-11.3-7.9l-6.5 5C9.5 39.6 16.2 44 24 44z" />
      <path fill="#1976D2" d="M43.6 20.5H42V20H24v8h11.3c-.8 2.2-2.2 4.2-4.1 5.6l6.2 5.2C37 39.2 44 34 44 24c0-1.3-.1-2.4-.4-3.5z" />
    </svg>
  );
}

export function SignIn({ info, onSignedIn }: { info: SessionInfo; onSignedIn: (info: SessionInfo) => void }) {
  const [server, setServer] = useState(info.server_url);
  const [email, setEmail] = useState("");
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [busy, setBusy] = useState<"google" | "email" | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function run(kind: "google" | "email") {
    setBusy(kind);
    setError(null);
    try {
      if (server !== info.server_url) await session.setServerUrl(server);
      const next = kind === "google" ? await session.signInGoogle() : await session.signInDev(email);
      onSignedIn(next);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="flex h-full flex-col items-center justify-center" data-tauri-drag-region>
      <div className="w-[320px]">
        <div className="mb-8 text-center">
          <img src="/app-icon.svg" alt="" className="mx-auto mb-4 h-14 w-14 dark:invert" />
          <h1 className="text-2xl font-semibold">Welcome to nanace</h1>
          <p className="mt-1 text-ink-2">Your team's knowledge, shared with Claude.</p>
        </div>

        <Button className="h-10 w-full" onClick={() => run("google")} disabled={busy !== null}>
          {busy === "google" ? <Spinner /> : <GoogleMark />}
          {busy === "google" ? "Finish signing in in your browser…" : "Continue with Google"}
        </Button>

        <form
          className="mt-3 space-y-2"
          onSubmit={(e) => {
            e.preventDefault();
            void run("email");
          }}
        >
          <Input
            type="email"
            placeholder="Email (dev sign-in)"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
          />
          <Button className="h-10 w-full" type="submit" disabled={busy !== null || !email.includes("@")}>
            {busy === "email" && <Spinner />}
            Continue with email
          </Button>
        </form>

        {error && <p className="mt-3 rounded-md bg-danger/10 px-3 py-2 text-sm text-danger selectable">{error}</p>}

        <button
          className="mt-6 flex items-center gap-1 text-xs text-ink-3 hover:text-ink-2"
          onClick={() => setShowAdvanced((v) => !v)}
        >
          {showAdvanced ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
          Server: {server}
        </button>
        {showAdvanced && (
          <Input className="mt-2" value={server} onChange={(e) => setServer(e.target.value)} spellCheck={false} />
        )}
      </div>
    </div>
  );
}

export function Onboarding({ onCreated }: { onCreated: () => void }) {
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function create() {
    setBusy(true);
    setError(null);
    try {
      const suffix = Math.random().toString(36).slice(2, 6);
      const org = await api.createOrg(`${slugify(name)}-${suffix}`.slice(0, 63), name.trim());
      await api.createTeam(org.id, "general", "General");
      onCreated();
    } catch (err) {
      setError(String(err instanceof Error ? err.message : err));
      setBusy(false);
    }
  }

  return (
    <div className="flex h-full items-center justify-center" data-tauri-drag-region>
      <form
        className="w-[360px]"
        onSubmit={(e) => {
          e.preventDefault();
          void create();
        }}
      >
        <h1 className="text-2xl font-semibold">Create a workspace</h1>
        <p className="mt-1 mb-6 text-ink-2">
          A workspace holds teamspaces of pages. You can invite people to each teamspace.
        </p>
        <Input autoFocus placeholder="My Company" value={name} onChange={(e) => setName(e.target.value)} />
        <Button variant="primary" className="mt-3 h-10 w-full" disabled={busy || !name.trim()}>
          {busy && <Spinner />}
          Continue
        </Button>
        {error && <p className="mt-3 text-sm text-danger">{error}</p>}
      </form>
    </div>
  );
}
