// Sign-in landing page: exchanges the one-time login code for a session
// token (kept in memory only) and manages personal access tokens.
"use strict";

const $ = (id) => document.getElementById(id);
let session = null;

const ERRORS = {
  access_denied: "Sign-in was cancelled.",
  invalid_state: "The sign-in link expired or was already used. Please try again.",
  email_not_verified: "Your Google email address is not verified.",
  domain_not_allowed: "Your Google account is not in an allowed Workspace domain.",
  account_conflict: "This email is already linked to a different Google account.",
  token_exchange_failed: "Google sign-in failed. Please try again.",
  invalid_id_token: "Google returned an invalid identity. Please try again.",
  server_error: "Something went wrong on the server.",
};

async function api(method, path, body) {
  const headers = { "Content-Type": "application/json" };
  if (session) headers.Authorization = `Bearer ${session}`;
  const resp = await fetch(path, { method, headers, body: body ? JSON.stringify(body) : undefined });
  if (!resp.ok) {
    const err = await resp.json().catch(() => ({}));
    throw new Error(err.message || resp.statusText);
  }
  return resp.status === 204 ? null : resp.json();
}

function cell(row, text, className) {
  const td = document.createElement("td");
  td.textContent = text;
  if (className) td.className = className;
  row.appendChild(td);
  return td;
}

const day = (iso) => (iso ? iso.slice(0, 10) : "never");

async function refresh() {
  const tokens = await api("GET", "/api/v1/me/tokens");
  const list = $("list");
  list.replaceChildren();
  for (const t of tokens) {
    const row = document.createElement("tr");
    if (t.revoked_at) row.className = "revoked";
    cell(row, t.name);
    cell(row, `${t.token_prefix}…`);
    cell(row, t.scope);
    cell(row, day(t.expires_at));
    cell(row, day(t.last_used_at));
    const actions = cell(row, "");
    if (!t.revoked_at) {
      const button = document.createElement("button");
      button.textContent = "Revoke";
      button.addEventListener("click", async () => {
        await api("DELETE", `/api/v1/me/tokens/${t.id}`);
        await refresh();
      });
      actions.appendChild(button);
    }
    list.appendChild(row);
  }
}

$("create").addEventListener("submit", async (event) => {
  event.preventDefault();
  const form = new FormData(event.target);
  try {
    const created = await api("POST", "/api/v1/me/tokens", {
      name: form.get("name"),
      scope: form.get("scope"),
      expires_in_days: Number(form.get("days")),
    });
    $("token").textContent = created.token;
    $("created").hidden = false;
    await refresh();
  } catch (err) {
    $("status").textContent = `Could not create token: ${err.message}`;
  }
});

async function main() {
  const params = new URLSearchParams(location.search);
  // Remove the one-time code from the address bar and history.
  history.replaceState(null, "", location.pathname);
  const error = params.get("error");
  const code = params.get("code");
  if (error) {
    $("status").textContent = ERRORS[error] || "Sign-in failed.";
    $("signin").hidden = false;
    return;
  }
  if (!code) {
    $("status").textContent = "Sign in to manage your access tokens.";
    $("signin").hidden = false;
    return;
  }
  try {
    session = (await api("POST", "/api/v1/auth/exchange", { code })).access_token;
    const me = await api("GET", "/api/v1/me");
    $("status").textContent = `Signed in as ${me.email}.`;
    $("tokens").hidden = false;
    await refresh();
  } catch (err) {
    $("status").textContent = "The sign-in link expired or was already used. Please sign in again.";
    $("signin").hidden = false;
  }
}

main();
