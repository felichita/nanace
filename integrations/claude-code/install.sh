#!/usr/bin/env bash
# Installs or updates the nanace Claude Code integration:
#   - the `nanace-mcp` binary (MCP server) from the latest GitHub release,
#   - the `/nanace` skill in ~/.claude/skills/nanace,
#   - optionally connects Claude Code to a nanace server (token + `claude mcp add`),
#   - with NANACE_P2P=1, the `nanace-p2p` daemon as a background service of
#     this user, so Claude gets the `p2p_*` tools without the desktop app.
#     NOT FOR A PERSON'S LAPTOP, see below.
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
#   NANACE_LOCAL_BIN_DIR  take the binaries from this directory (e.g. target/release
#                      of a clone) instead of a release
#   NANACE_P2P         1 = also install and start the P2P daemon (see below)
#   NANACE_P2P_TEAMS   teams the daemon takes part in, `<org id>:<team id>:<accept mode>`
#                      comma separated (default: ask, with accept mode `team`)
#   NANACE_P2P_TOKEN_FILE  the daemon's `p2p`-scoped token (default: ~/.config/nanace/p2p.token)
#   NANACE_P2P_DIR     the daemon's state: peer key, socket, log (default: ~/.local/share/nanace/p2p)
#   NANACE_REF         git ref the skill is installed from (default: main)
#   NANACE_BASE_URL    override the skill download base (e.g. file:// of a clone)
#   CLAUDE_CONFIG_DIR  Claude Code config dir (default: ~/.claude)
#
# DO NOT USE NANACE_P2P=1 ON A PERSON'S LAPTOP. It installs a service that
# restarts on its own (launchd KeepAlive, systemd Restart), accepts requests
# without asking and lets the agent answer with nobody looking at the answer.
# A laptop someone works on uses the nanace desktop app only: it asks before a
# request reaches the agent and shows each answer before it leaves.
#
# NANACE_P2P=1 is for a machine nobody sits at. It runs the daemon the way an
# agent in a container runs it, within the team's `Policy: P2P`: the team
# owner has to list the account under `server_peers` and set
# `max_accept_mode: team`.
set -euo pipefail

REPO="felichita/nanace"
REF="${NANACE_REF:-main}"
SKILL_URL="${NANACE_BASE_URL:-https://raw.githubusercontent.com/$REPO/$REF/integrations/claude-code/skills/nanace}"
CONFIG_DIR="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
SKILL_DIR="$CONFIG_DIR/skills/nanace"
SKILL_FILES=(SKILL.md schema-template.md)
BIN_DIR="${NANACE_BIN_DIR:-$HOME/.local/bin}"
BIN="$BIN_DIR/nanace-mcp"
P2P_BIN="$BIN_DIR/nanace-p2p"
BINARIES=(nanace-mcp)
if [ "${NANACE_P2P:-0}" = 1 ]; then BINARIES+=(nanace-p2p); fi
API_URL="${NANACE_API_URL:-http://127.0.0.1:8080}"
API_URL="${API_URL%/}"
TOKEN_FILE="${NANACE_TOKEN_FILE:-${XDG_CONFIG_HOME:-$HOME/.config}/nanace/claude-code.token}"
P2P_TOKEN_FILE="${NANACE_P2P_TOKEN_FILE:-${XDG_CONFIG_HOME:-$HOME/.config}/nanace/p2p.token}"
P2P_DIR="${NANACE_P2P_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/nanace/p2p}"
P2P_LABEL="dev.nanace.p2p"
# Set once the daemon answers on its socket.
P2P_READY=0

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

# True when the installed binary $1 is the one recorded for release $2.
# `.<name>.version` records which release a binary came from and its checksum.
binary_current() {
  local bin="$BIN_DIR/$1" marker="$BIN_DIR/.$1.version" tag="" sum=""
  if [ -f "$marker" ]; then read -r tag sum < "$marker" || true; fi
  [ -x "$bin" ] && [ "$tag" = "$2" ] && [ "$(sha256 "$bin")" = "$sum" ]
}

# place_binary <name> <tag> <file>: installs <file> as $BIN_DIR/<name>.
place_binary() {
  local name="$1" tag="$2" src="$3" bin="$BIN_DIR/$1" marker="$BIN_DIR/.$1.version"
  local have marker_tag="" existed=0
  if [ -f "$marker" ]; then read -r marker_tag _ < "$marker" || true; fi
  have="$(sha256 "$src")"
  mkdir -p "$BIN_DIR"
  if [ -x "$bin" ] && [ "$(sha256 "$bin")" = "$have" ]; then
    say "$name $tag is already installed: $bin"
  else
    [ -e "$bin" ] && existed=1
    # Copy next to the target and rename, so a running binary is never half-written.
    cp "$src" "$bin.new"
    chmod 755 "$bin.new"
    mv -f "$bin.new" "$bin"
    if [ "$marker_tag" = "$tag" ]; then
      say "Reinstalled $name $tag (the installed file did not match): $bin"
    elif [ -n "$marker_tag" ]; then
      say "Updated $name $marker_tag -> $tag: $bin"
    elif [ "$existed" = 1 ]; then
      say "Replaced an unmanaged $name with $tag: $bin"
    else
      say "Installed $name $tag: $bin"
    fi
  fi
  printf '%s %s\n' "$tag" "$have" > "$marker"
}

path_note() {
  case ":$PATH:" in *":$BIN_DIR:"*) ;; *) say "Note: $BIN_DIR is not on your PATH (Claude uses the absolute path, so this is optional)." ;; esac
}

install_binaries() {
  local target tag name archive want b missing=0
  if [ -n "${NANACE_LOCAL_BIN_DIR:-}" ]; then
    for b in "${BINARIES[@]}"; do
      [ -f "$NANACE_LOCAL_BIN_DIR/$b" ] || die "$b not found in $NANACE_LOCAL_BIN_DIR (NANACE_LOCAL_BIN_DIR)"
      place_binary "$b" local "$NANACE_LOCAL_BIN_DIR/$b"
    done
    path_note
    return 0
  fi
  if ! target="$(release_target)"; then
    warn "no nanace release build for $(uname -s) $(uname -m); skipping the binaries"
    warn "use the nanace desktop app (it bundles them) or: cargo build --release -p nanace-mcp"
    return 0
  fi
  tag="${NANACE_VERSION:-}"
  if [ -z "$tag" ]; then
    tag="$(latest_tag)" || die "could not find the latest nanace release"
  fi
  case "$tag" in v*) ;; *) tag="v$tag" ;; esac

  for b in "${BINARIES[@]}"; do
    if binary_current "$b" "$tag"; then
      say "$b $tag is up to date: $BIN_DIR/$b"
    else
      missing=1
    fi
  done
  [ "$missing" = 1 ] || return 0

  name="nanace-${tag#v}-$target"
  archive="$TMP/$name.tar.gz"
  say "Downloading nanace $tag ($target)..."
  fetch "https://github.com/$REPO/releases/download/$tag/$name.tar.gz" "$archive" \
    || die "could not download $name.tar.gz from release $tag"
  fetch "https://github.com/$REPO/releases/download/$tag/$name.tar.gz.sha256" "$archive.sha256" \
    || die "could not download the checksum for $name.tar.gz"
  want="$(cut -d' ' -f1 < "$archive.sha256")"
  [ "$(sha256 "$archive")" = "$want" ] || die "checksum mismatch for $name.tar.gz"
  for b in "${BINARIES[@]}"; do
    if binary_current "$b" "$tag"; then continue; fi
    if ! tar -xzf "$archive" -C "$TMP" "$name/$b" 2>/dev/null; then
      [ "$b" = nanace-p2p ] || die "$b not found in $name.tar.gz"
      warn "release $tag has no nanace-p2p; P2P needs a newer release (NANACE_VERSION)"
      warn "or binaries you built yourself (NANACE_LOCAL_BIN_DIR)"
      continue
    fi
    place_binary "$b" "$tag" "$TMP/$name/$b"
  done
  path_note
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

# api_get <path> [token file]: API GET authenticated with the token in the
# file (default $TOKEN_FILE). The header is passed to curl through a file, so
# the token never appears in process arguments.
api_get() {
  local hdr="$TMP/auth.header" token status=0
  token="$(tr -d '[:space:]' < "${2:-$TOKEN_FILE}")"
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

# prompt_token <file> <me.json> <step 2 text> <what for>: reads a new token
# (hidden input), stores it in <file> (mode 600) and saves /me to <me.json>.
prompt_token() {
  local file="$1" me="$2" token tries=0
  say "Create an access token for $4:"
  say "  1. Open $API_URL/auth/complete and sign in."
  say "  2. $3"
  say "  3. Copy it and paste it here (input is hidden)."
  open_browser "$API_URL/auth/complete"
  mkdir -p "$(dirname "$file")"
  chmod 700 "$(dirname "$file")" 2>/dev/null || true
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
    (umask 077 && printf '%s\n' "$token" > "$file.new")
    mv -f "$file.new" "$file"
    unset token
    if api_get /api/v1/me "$file" > "$me" 2>/dev/null; then return 0; fi
    warn "the server rejected this token (invalid, expired or revoked); try again"
  done
  rm -f "$file"
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
  # With the daemon up, connect again: the registration needs its socket.
  if [ "$cur" = "$BIN" ] && [ "${NANACE_RECONNECT:-0}" != 1 ] && [ "$P2P_READY" != 1 ]; then
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
    prompt_token "$TOKEN_FILE" "$TMP/me.json" \
      "Create a token: scope 'write' (needed to save pages), any name, up to 365 days." "Claude Code" \
      || { warn "no valid token; Claude Code was not connected"; manual_connect_help; return 0; }
  fi
  chmod 600 "$TOKEN_FILE"

  email="$(sed -n 's/.*"email":"\([^"]*\)".*/\1/p' "$TMP/me.json")"
  scope="$(token_scope || true)"
  if [ "$scope" = read ]; then
    warn "this token has 'read' scope: Claude can search but /nanace cannot save pages"
  fi
  grep -qE '"team_role":"(agent|writer|admin)"' "$TMP/me.json" \
    || warn "you are not a writer in any team yet: /nanace can only save after a team admin adds you"
  org="$(choose_org)"

  if [ -n "$cur" ]; then claude mcp remove nanace --scope user >/dev/null 2>&1 || true; fi
  local args=(mcp add nanace --scope user --env "NANACE_API_URL=$API_URL" --env "NANACE_TOKEN_FILE=$TOKEN_FILE")
  [ "$scope" = read ] || args+=(--env NANACE_ALLOW_WRITES=true)
  [ -z "$org" ] || args+=(--env "NANACE_ORG_ID=$org")
  if [ "$P2P_READY" = 1 ]; then
    args+=(--env NANACE_ALLOW_P2P=true --env "NANACE_P2P_SOCKET=$P2P_DIR/p2p.sock"
      --env "NANACE_P2P_SECRET_FILE=$P2P_DIR/ipc.secret")
  fi
  args+=(-- "$BIN")
  claude "${args[@]}" >/dev/null || { warn "claude mcp add failed"; manual_connect_help; return 0; }
  say "Connected Claude Code to $API_URL as ${email:-you} (token file: $TOKEN_FILE)."
  [ "$P2P_READY" != 1 ] || say "Claude has the p2p_* tools through the daemon in $P2P_DIR."
  # Capture first: with pipefail, `grep -q` closing the pipe early would fail it.
  local status_out
  status_out="$(claude mcp get nanace 2>/dev/null || true)"
  if printf '%s\n' "$status_out" | grep -q 'Connected'; then
    say "Claude Code reports the nanace server as connected."
  else
    warn "Claude Code does not report the server as connected yet; check: claude mcp get nanace"
  fi
}

# --- P2P daemon ------------------------------------------------------------

p2p_help() {
  say "P2P was not set up. What it needs:"
  say "  - a server that runs a P2P node (NANACE_P2P_BOOTSTRAP on the server),"
  say "  - a page 'Policy: P2P' in the team, with your account under server_peers.users"
  say "    and max_accept_mode: team (this daemon has no window to ask you),"
  say "  - a token with scope 'p2p' in $P2P_TOKEN_FILE."
  say "Run the installer again with NANACE_P2P=1 once these are in place."
}

# Prints the chosen teams as `<org id>:<team id>:team,...`.
choose_p2p_teams() {
  local rows org team name role picked=""
  rows="$(awk '{ gsub(/\},\{/, "}\n{"); print }' "$TMP/p2p-me.json" | sed -n \
    's/.*"org_id":"\([^"]*\)","org_slug":"\([^"]*\)".*"team_id":"\([^"]*\)","team_slug":"\([^"]*\)","team_role":"\([^"]*\)".*/\1 \3 \2\/\4 \5/p')"
  [ -n "$rows" ] || return 0
  while read -r org team name role; do
    if ask "Take part in P2P in team $name (your role: $role)?" y; then
      picked="${picked:+$picked,}$org:$team:team"
    fi
  done <<< "$rows"
  printf '%s\n' "$picked"
}

xml_escape() { printf '%s' "$1" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'; }

# start_p2p_service KEY=VALUE...: runs `nanace-p2p daemon` as a service of
# this user with that environment, replacing a previous one.
start_p2p_service() {
  local kv unit plist
  case "$(uname -s)" in
    Darwin)
      plist="$HOME/Library/LaunchAgents/$P2P_LABEL.plist"
      mkdir -p "$(dirname "$plist")"
      {
        printf '%s\n' '<?xml version="1.0" encoding="UTF-8"?>' \
          '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">' \
          '<plist version="1.0"><dict>' \
          "<key>Label</key><string>$P2P_LABEL</string>" \
          "<key>ProgramArguments</key><array><string>$(xml_escape "$P2P_BIN")</string><string>daemon</string></array>" \
          '<key>EnvironmentVariables</key><dict>'
        for kv in "$@"; do
          printf '<key>%s</key><string>%s</string>\n' "${kv%%=*}" "$(xml_escape "${kv#*=}")"
        done
        printf '%s\n' '</dict>' \
          '<key>RunAtLoad</key><true/>' '<key>KeepAlive</key><true/>' '<key>ThrottleInterval</key><integer>30</integer>' \
          "<key>StandardErrorPath</key><string>$(xml_escape "$P2P_DIR/daemon.log")</string>" \
          '</dict></plist>'
      } > "$plist"
      launchctl bootout "gui/$(id -u)/$P2P_LABEL" >/dev/null 2>&1 || true
      launchctl bootstrap "gui/$(id -u)" "$plist" || return 1
      say "Service: $plist (stop it with: launchctl bootout gui/$(id -u)/$P2P_LABEL)"
      ;;
    Linux)
      systemctl --user show-environment >/dev/null 2>&1 || { warn "no systemd user session to run the daemon in"; return 1; }
      unit="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/nanace-p2p.service"
      mkdir -p "$(dirname "$unit")"
      {
        printf '%s\n' '[Unit]' 'Description=nanace P2P daemon' 'After=network-online.target' '' '[Service]'
        for kv in "$@"; do printf 'Environment="%s"\n' "$(printf '%s' "$kv" | sed 's/%/%%/g')"; done
        printf '%s\n' "ExecStart=\"$P2P_BIN\" daemon" "StandardError=append:$P2P_DIR/daemon.log" \
          'Restart=on-failure' 'RestartSec=30' '' '[Install]' 'WantedBy=default.target'
      } > "$unit"
      systemctl --user daemon-reload || return 1
      systemctl --user enable nanace-p2p.service >/dev/null 2>&1 || true
      systemctl --user restart nanace-p2p.service || return 1
      say "Service: $unit (stop it with: systemctl --user disable --now nanace-p2p)"
      ;;
    *) warn "no service setup for $(uname -s)"; return 1 ;;
  esac
}

setup_p2p() {
  local bootstrap teams i=0
  say ""
  warn "NANACE_P2P=1 is not for a laptop a person works on: this daemon accepts requests"
  warn "and answers them with nobody asked. On such a laptop use the nanace desktop app."
  if [ ! -x "$P2P_BIN" ]; then
    say "nanace-p2p is not installed here, so P2P was not set up."
    return 0
  fi
  if ! curl -fsS -o /dev/null --max-time 10 "$API_URL/livez" 2>/dev/null; then
    warn "cannot reach $API_URL (set NANACE_API_URL to your nanace server)"
    p2p_help
    return 0
  fi
  if [ -s "$P2P_TOKEN_FILE" ] && api_get /api/v1/me "$P2P_TOKEN_FILE" > "$TMP/p2p-me.json" 2>/dev/null; then
    say "Using the P2P token in $P2P_TOKEN_FILE."
  elif has_tty; then
    prompt_token "$P2P_TOKEN_FILE" "$TMP/p2p-me.json" \
      "Create a token: scope 'p2p' (it can only do P2P, not read pages), any name." "the P2P daemon" \
      || { warn "no valid token"; p2p_help; return 0; }
  else
    say "No terminal to ask for a token."
    p2p_help
    return 0
  fi
  chmod 600 "$P2P_TOKEN_FILE"

  bootstrap="$(api_get /api/v1/p2p/config "$P2P_TOKEN_FILE" 2>/dev/null | sed -n 's/.*"bootstrap":"\([^"]*\)".*/\1/p')" || true
  if [ -z "$bootstrap" ]; then
    warn "the server at $API_URL runs no P2P node"
    p2p_help
    return 0
  fi

  teams="${NANACE_P2P_TEAMS:-}"
  if [ -z "$teams" ] && has_tty; then teams="$(choose_p2p_teams)"; fi
  if [ -z "$teams" ]; then
    warn "no team chosen (set NANACE_P2P_TEAMS=<org id>:<team id>:team)"
    p2p_help
    return 0
  fi

  mkdir -p "$P2P_DIR"
  chmod 700 "$P2P_DIR"
  : > "$P2P_DIR/daemon.log"
  start_p2p_service "NANACE_API_URL=$API_URL" "NANACE_TOKEN_FILE=$P2P_TOKEN_FILE" "NANACE_P2P_DIR=$P2P_DIR" \
    "NANACE_P2P_BOOTSTRAP=$bootstrap" "NANACE_P2P_KIND=server" "NANACE_P2P_TEAMS=$teams" \
    || { warn "could not start the daemon as a service"; p2p_help; return 0; }

  while [ "$i" -lt 20 ]; do
    if NANACE_P2P_DIR="$P2P_DIR" "$P2P_BIN" call whoami > "$TMP/whoami.json" 2>/dev/null; then
      P2P_READY=1
      say "The P2P daemon is running: peer $(sed -n 's/.*"peer_id": *"\([^"]*\)".*/\1/p' "$TMP/whoami.json" | head -n 1)"
      say "Check it any time: NANACE_P2P_DIR=$P2P_DIR $P2P_BIN call whoami"
      return 0
    fi
    i=$((i + 1))
    sleep 1
  done
  warn "the daemon did not come up; the end of $P2P_DIR/daemon.log:"
  tail -n 5 "$P2P_DIR/daemon.log" >&2 || true
  p2p_help
}

if [ "${NANACE_SKIP_BINARY:-0}" = 1 ]; then
  say "Skipping the binaries (NANACE_SKIP_BINARY=1)."
else
  install_binaries
fi
install_skill
if [ "${NANACE_SKIP_BINARY:-0}" != 1 ]; then
  if [ "${NANACE_P2P:-0}" = 1 ]; then setup_p2p; fi
  connect_claude
fi
say ""
say "Restart Claude Code to pick up changes, then run: /nanace <TASK-KEY>"
