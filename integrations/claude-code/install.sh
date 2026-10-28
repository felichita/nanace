#!/usr/bin/env bash
# Installs or updates the nanace Claude Code integration:
#   - the `nanace-mcp` binary (MCP server) from the latest GitHub release,
#   - the `/nanace` skill in ~/.claude/skills/nanace,
#   - optionally connects Claude Code to a nanace server (token + `claude mcp add`).
#
#   /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/felichita/nanace/HEAD/integrations/claude-code/install.sh)"
#
# Environment:
#   NANACE_API_URL     nanace server to connect to (default: http://127.0.0.1:8080)
#   NANACE_TOKEN_FILE  where the access token is kept (default: ~/.config/nanace/claude-code.token)
#   NANACE_ORG_ID      organization to use when you belong to several (default: ask)
#   NANACE_CONNECT     0 = do not connect Claude Code, only report its state
#   NANACE_RECONNECT   1 = connect again even if Claude Code already uses this binary
#   NANACE_VERSION     release tag for nanace-mcp, e.g. v0.1.3 (default: latest release)
#   NANACE_BIN_DIR     where nanace-mcp goes (default: ~/.local/bin)
#   NANACE_SKIP_BINARY 1 = install only the skill (implies no connect)
#   NANACE_REF         git ref the skill is installed from (default: main)
#   NANACE_BASE_URL    override the skill download base (e.g. file:// of a clone)
#   CLAUDE_CONFIG_DIR  Claude Code config dir (default: ~/.claude)
set -euo pipefail

REPO="felichita/nanace"
REF="${NANACE_REF:-main}"
SKILL_URL="${NANACE_BASE_URL:-https://raw.githubusercontent.com/$REPO/$REF/integrations/claude-code/skills/nanace}"
CONFIG_DIR="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
SKILL_DIR="$CONFIG_DIR/skills/nanace"
SKILL_FILES=(SKILL.md schema-template.md)
BIN_DIR="${NANACE_BIN_DIR:-$HOME/.local/bin}"
BIN="$BIN_DIR/nanace-mcp"
# Records which release the installed binary came from and its checksum.
MARKER="$BIN_DIR/.nanace-mcp.version"
API_URL="${NANACE_API_URL:-http://127.0.0.1:8080}"
API_URL="${API_URL%/}"
TOKEN_FILE="${NANACE_TOKEN_FILE:-${XDG_CONFIG_HOME:-$HOME/.config}/nanace/claude-code.token}"

say() { printf '%s\n' "$*"; }
warn() { printf 'warning: %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || die "curl is required"
fetch() { curl -fsSL "$1" -o "$2"; }

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# --- nanace-mcp ------------------------------------------------------------

release_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"
  case "$arch" in
    arm64 | aarch64) arch=aarch64 ;;
    x86_64 | amd64) arch=x86_64 ;;
    *) return 1 ;;
  esac
  case "$os" in
    Darwin)
      # A shell under Rosetta reports x86_64 on Apple silicon.
      if [ "$arch" = x86_64 ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = 1 ]; then
        arch=aarch64
      fi
      [ "$arch" = aarch64 ] || return 1 # no Intel macOS build is published
      echo "aarch64-apple-darwin" ;;
    Linux)
      if ldd --version 2>&1 | grep -qi musl; then return 1; fi # builds are glibc only
      echo "$arch-unknown-linux-gnu" ;;
    *) return 1 ;;
  esac
}

latest_tag() {
  local url
  url="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")" || return 1
  case "$url" in */tag/*) echo "${url##*/}" ;; *) return 1 ;; esac
}

install_binary() {
  local target tag marker_tag="" marker_sum="" name archive have want
  if ! target="$(release_target)"; then
    warn "no nanace-mcp release build for $(uname -s) $(uname -m); skipping the binary"
    warn "use the nanace desktop app (it bundles nanace-mcp) or: cargo build --release -p nanace-mcp"
    return 0
  fi
  tag="${NANACE_VERSION:-}"
  if [ -z "$tag" ]; then
    tag="$(latest_tag)" || die "could not find the latest nanace release"
  fi
  case "$tag" in v*) ;; *) tag="v$tag" ;; esac

  if [ -f "$MARKER" ]; then
    read -r marker_tag marker_sum < "$MARKER" || true
  fi
  if [ -x "$BIN" ] && [ "$marker_tag" = "$tag" ] && [ "$(sha256 "$BIN")" = "$marker_sum" ]; then
    say "nanace-mcp $tag is up to date: $BIN"
    return 0
  fi

  name="nanace-${tag#v}-$target"
  archive="$TMP/$name.tar.gz"
  say "Downloading nanace-mcp $tag ($target)..."
  fetch "https://github.com/$REPO/releases/download/$tag/$name.tar.gz" "$archive" \
    || die "could not download $name.tar.gz from release $tag"
  fetch "https://github.com/$REPO/releases/download/$tag/$name.tar.gz.sha256" "$archive.sha256" \
    || die "could not download the checksum for $name.tar.gz"
  want="$(cut -d' ' -f1 < "$archive.sha256")"
  [ "$(sha256 "$archive")" = "$want" ] || die "checksum mismatch for $name.tar.gz"
  tar -xzf "$archive" -C "$TMP" "$name/nanace-mcp" || die "nanace-mcp not found in $name.tar.gz"

  have="$(sha256 "$TMP/$name/nanace-mcp")"
  mkdir -p "$BIN_DIR"
  if [ -x "$BIN" ] && [ "$(sha256 "$BIN")" = "$have" ]; then
    say "nanace-mcp $tag is already installed: $BIN"
  else
    local existed=0
    [ -e "$BIN" ] && existed=1
    # Copy next to the target and rename, so a running nanace-mcp is never half-written.
    cp "$TMP/$name/nanace-mcp" "$BIN.new"
    chmod 755 "$BIN.new"
    mv -f "$BIN.new" "$BIN"
    if [ "$marker_tag" = "$tag" ]; then
      say "Reinstalled nanace-mcp $tag (the installed file did not match): $BIN"
    elif [ -n "$marker_tag" ]; then
      say "Updated nanace-mcp $marker_tag -> $tag: $BIN"
    elif [ "$existed" = 1 ]; then
      say "Replaced an unmanaged nanace-mcp with $tag: $BIN"
    else
      say "Installed nanace-mcp $tag: $BIN"
    fi
  fi
  printf '%s %s\n' "$tag" "$have" > "$MARKER"

  case ":$PATH:" in *":$BIN_DIR:"*) ;; *) say "Note: $BIN_DIR is not on your PATH (Claude uses the absolute path, so this is optional)." ;; esac
}

# --- skill -----------------------------------------------------------------

install_skill() {
  local f changed=0 backup
  say "Downloading the /nanace skill ($REF)..."
  mkdir -p "$TMP/skill"
  for f in "${SKILL_FILES[@]}"; do
    fetch "$SKILL_URL/$f" "$TMP/skill/$f" || die "could not download $SKILL_URL/$f"
    [ -s "$TMP/skill/$f" ] || die "$f is empty"
  done
  head -n 1 "$TMP/skill/SKILL.md" | grep -qx -- '---' || die "SKILL.md does not look like a skill file"

  if [ -d "$SKILL_DIR" ]; then
    for f in "${SKILL_FILES[@]}"; do
      cmp -s "$TMP/skill/$f" "$SKILL_DIR/$f" || changed=1
    done
    if [ "$changed" -eq 0 ]; then
      say "/nanace skill is up to date: $SKILL_DIR"
      return 0
    fi
    # Outside skills/, or Claude Code would load the backup as a second skill.
    backup="$CONFIG_DIR/skill-backups/nanace-$(date +%Y%m%d%H%M%S)"
    mkdir -p "$CONFIG_DIR/skill-backups"
    mv "$SKILL_DIR" "$backup"
    say "Previous skill moved to $backup"
  fi
  mkdir -p "$SKILL_DIR"
  for f in "${SKILL_FILES[@]}"; do cp "$TMP/skill/$f" "$SKILL_DIR/$f"; done
  say "Installed the /nanace skill: $SKILL_DIR"
}

# --- Claude Code connection ------------------------------------------------

# Prompts read from the terminal, so they also work when the script is piped.
has_tty() { { : < /dev/tty; } 2>/dev/null; }
ask() { # ask "question" default(y|n) -> 0 for yes
  local answer hint="[y/N]"
  [ "$2" = y ] && hint="[Y/n]"
  printf '%s %s ' "$1" "$hint" > /dev/tty
  IFS= read -r answer < /dev/tty || answer=""
  case "${answer:-$2}" in y | Y | yes | Yes) return 0 ;; *) return 1 ;; esac
}

registered_command() {
  claude mcp get nanace 2>/dev/null | sed -n 's/^ *Command: //p' | head -n 1
}

# API GET authenticated with the token in $TOKEN_FILE. The header is passed to
# curl through a file, so the token never appears in process arguments.
api_get() {
  local hdr="$TMP/auth.header" token status=0
  token="$(tr -d '[:space:]' < "$TOKEN_FILE")"
  (umask 077 && printf 'Authorization: Bearer %s\n' "$token" > "$hdr")
  curl -fsS --max-time 20 -H @"$hdr" "$API_URL$1" || status=$?
  rm -f "$hdr"
  return "$status"
}

open_browser() {
  if command -v open >/dev/null 2>&1 && [ "$(uname -s)" = Darwin ]; then open "$1" >/dev/null 2>&1 || true
  elif command -v xdg-open >/dev/null 2>&1 && [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then xdg-open "$1" >/dev/null 2>&1 || true
  fi
}

# Reads a new token (hidden input) and stores it in $TOKEN_FILE (mode 600).
prompt_token() {
  local token tries=0
  say "Create an access token for Claude Code:"
  say "  1. Open $API_URL/auth/complete and sign in."
  say "  2. Create a token: scope 'write' (needed to save pages), any name, up to 365 days."
  say "  3. Copy it and paste it here (input is hidden)."
  open_browser "$API_URL/auth/complete"
  mkdir -p "$(dirname "$TOKEN_FILE")"
  chmod 700 "$(dirname "$TOKEN_FILE")" 2>/dev/null || true
  while [ "$tries" -lt 3 ]; do
    tries=$((tries + 1))
    printf 'Token: ' > /dev/tty
    IFS= read -rs token < /dev/tty || token=""
    printf '\n' > /dev/tty
    token="$(printf '%s' "$token" | tr -d '[:space:]')"
    case "$token" in
      nnc_pat_?*) ;;
      *) warn "that does not look like a nanace token (it starts with nnc_pat_)"; continue ;;
    esac
    (umask 077 && printf '%s\n' "$token" > "$TOKEN_FILE.new")
    mv -f "$TOKEN_FILE.new" "$TOKEN_FILE"
    unset token
    if api_get /api/v1/me > "$TMP/me.json" 2>/dev/null; then return 0; fi
    warn "the server rejected this token (invalid, expired or revoked); try again"
  done
  rm -f "$TOKEN_FILE"
  return 1
}

# Prints "read" or "write" for the token in $TOKEN_FILE, or nothing if unknown.
token_scope() {
  local prefix scopes
  # The API shows the first 12 characters (`nnc_pat_` + 4) of each token.
  prefix="$(tr -d '[:space:]' < "$TOKEN_FILE" | cut -c1-12)"
  scopes="$(api_get /api/v1/me/tokens 2>/dev/null | awk '{ gsub(/\},\{/, "}\n{"); print }' \
    | grep -F "\"token_prefix\":\"$prefix\"" | grep -F '"revoked_at":null' \
    | sed -n 's/.*"scope":"\([a-z]*\)".*/\1/p' | sort -u)" || true
  # Only answer when the prefix identifies one scope.
  if [ "$(printf '%s\n' "$scopes" | grep -c .)" -eq 1 ]; then printf '%s\n' "$scopes"; fi
}

# Picks the organization: NANACE_ORG_ID, the only one, or asks.
choose_org() {
  local orgs count i=0 pick line
  orgs="$(grep -o '"org_id":"[^"]*","org_slug":"[^"]*"' "$TMP/me.json" | sort -u \
    | sed 's/"org_id":"\([^"]*\)","org_slug":"\([^"]*\)"/\1 \2/')"
  count="$(printf '%s' "$orgs" | grep -c . || true)"
  if [ -n "${NANACE_ORG_ID:-}" ]; then echo "$NANACE_ORG_ID"; return 0; fi
  if [ "$count" -eq 0 ]; then
    warn "your account is not a member of any organization yet: ask an org admin to add you"
    warn "(and to a team as writer to save pages); the connection will work once you are added"
    return 0
  fi
  if [ "$count" -eq 1 ]; then printf '%s\n' "$orgs" | cut -d' ' -f1; return 0; fi
  say "You belong to several organizations:" > /dev/tty
  while IFS= read -r line; do i=$((i + 1)); printf '  %d) %s\n' "$i" "${line#* }" > /dev/tty; done <<< "$orgs"
  printf 'Default organization for Claude [1]: ' > /dev/tty
  IFS= read -r pick < /dev/tty || pick=""
  pick="${pick:-1}"
  case "$pick" in *[!0-9]* | 0) pick=1 ;; esac
  [ "$pick" -le "$count" ] || pick=1
  printf '%s\n' "$orgs" | sed -n "${pick}p" | cut -d' ' -f1
}

manual_connect_help() {
  say "To connect later: run this installer again in a terminal, or set it up by hand:"
  say "  1. Create a 'write' token at $API_URL/auth/complete and save it to $TOKEN_FILE (chmod 600)."
  say "  2. claude mcp add nanace --scope user --env NANACE_API_URL=$API_URL \\"
  say "       --env NANACE_TOKEN_FILE=$TOKEN_FILE --env NANACE_ALLOW_WRITES=true -- $BIN"
  say "Or use 'Connect to Claude' in the nanace desktop app (it registers its own nanace-mcp)."
}

connect_claude() {
  local cur scope org email
  say ""
  if ! command -v claude >/dev/null 2>&1; then
    say "Claude Code CLI not found; install it and run this installer again to connect."
    return 0
  fi
  if [ ! -x "$BIN" ]; then
    say "nanace-mcp is not installed here, so Claude Code cannot be connected to it."
    return 0
  fi
  cur="$(registered_command || true)"
  if [ "$cur" = "$BIN" ] && [ "${NANACE_RECONNECT:-0}" != 1 ]; then
    say "Claude Code is connected through this nanace-mcp: $BIN"
    say "(NANACE_RECONNECT=1 connects again, e.g. with a new token or server.)"
    return 0
  fi
  if [ -n "$cur" ] && [ "$cur" != "$BIN" ]; then
    say "Claude Code currently runs a different nanace-mcp: $cur"
    say "(that copy is not managed by this installer; the desktop app updates its own)"
  fi
  if [ "${NANACE_CONNECT:-1}" = 0 ]; then manual_connect_help; return 0; fi
  if ! has_tty; then say "No terminal to ask for a token."; manual_connect_help; return 0; fi
  if [ "$cur" = "$BIN" ]; then
    ask "Connect Claude Code again to the nanace server at $API_URL?" y || return 0
  elif [ -n "$cur" ]; then
    ask "Switch Claude Code to $BIN and $API_URL?" n || return 0
  else
    ask "Connect Claude Code to the nanace server at $API_URL?" y || { manual_connect_help; return 0; }
  fi

  if ! curl -fsS -o /dev/null --max-time 10 "$API_URL/livez" 2>/dev/null; then
    warn "cannot reach $API_URL (set NANACE_API_URL to your nanace server)"
    manual_connect_help
    return 0
  fi

  if [ -s "$TOKEN_FILE" ] && api_get /api/v1/me > "$TMP/me.json" 2>/dev/null \
    && ask "Use the token saved in $TOKEN_FILE?" y; then
    :
  else
    prompt_token || { warn "no valid token; Claude Code was not connected"; manual_connect_help; return 0; }
  fi
  chmod 600 "$TOKEN_FILE"

  email="$(sed -n 's/.*"email":"\([^"]*\)".*/\1/p' "$TMP/me.json")"
  scope="$(token_scope || true)"
  if [ "$scope" = read ]; then
    warn "this token has 'read' scope: Claude can search but /nanace cannot save pages"
  fi
  grep -qE '"team_role":"(writer|admin)"' "$TMP/me.json" \
    || warn "you are not a writer in any team yet: /nanace can only save after a team admin adds you"
  org="$(choose_org)"

  if [ -n "$cur" ]; then claude mcp remove nanace --scope user >/dev/null 2>&1 || true; fi
  local args=(mcp add nanace --scope user --env "NANACE_API_URL=$API_URL" --env "NANACE_TOKEN_FILE=$TOKEN_FILE")
  [ "$scope" = read ] || args+=(--env NANACE_ALLOW_WRITES=true)
  [ -z "$org" ] || args+=(--env "NANACE_ORG_ID=$org")
  args+=(-- "$BIN")
  claude "${args[@]}" >/dev/null || { warn "claude mcp add failed"; manual_connect_help; return 0; }
  say "Connected Claude Code to $API_URL as ${email:-you} (token file: $TOKEN_FILE)."
  # Capture first: with pipefail, `grep -q` closing the pipe early would fail it.
  local status_out
  status_out="$(claude mcp get nanace 2>/dev/null || true)"
  if printf '%s\n' "$status_out" | grep -q 'Connected'; then
    say "Claude Code reports the nanace server as connected."
  else
    warn "Claude Code does not report the server as connected yet; check: claude mcp get nanace"
  fi
}

if [ "${NANACE_SKIP_BINARY:-0}" = 1 ]; then
  say "Skipping nanace-mcp (NANACE_SKIP_BINARY=1)."
else
  install_binary
fi
install_skill
[ "${NANACE_SKIP_BINARY:-0}" = 1 ] || connect_claude
say ""
say "Restart Claude Code to pick up changes, then run: /nanace <TASK-KEY>"
