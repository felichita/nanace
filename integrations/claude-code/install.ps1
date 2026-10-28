# Installs or updates the nanace Claude Code integration on Windows:
#   - the `nanace-mcp.exe` binary (MCP server) from the latest GitHub release,
#   - the `/nanace` skill in %USERPROFILE%\.claude\skills\nanace,
#   - optionally connects Claude Code to a nanace server (token + `claude mcp add`).
#
#   powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/felichita/nanace/HEAD/integrations/claude-code/install.ps1 | iex"
#
# Environment:
#   NANACE_API_URL     nanace server to connect to (default: http://127.0.0.1:8080)
#   NANACE_TOKEN_FILE  where the access token is kept (default: %LOCALAPPDATA%\nanace\claude-code.token)
#   NANACE_ORG_ID      organization to use when you belong to several (default: ask)
#   NANACE_CONNECT     0 = do not connect Claude Code, only report its state
#   NANACE_RECONNECT   1 = connect again even if Claude Code already uses this binary
#   NANACE_VERSION     release tag for nanace-mcp, e.g. v0.1.3 (default: latest release)
#   NANACE_BIN_DIR     where nanace-mcp.exe goes (default: %LOCALAPPDATA%\nanace\bin)
#   NANACE_SKIP_BINARY 1 = install only the skill (implies no connect)
#   NANACE_REF         git ref the skill is installed from (default: main)
#   NANACE_BASE_URL    override the skill download base
#   CLAUDE_CONFIG_DIR  Claude Code config dir (default: ~\.claude)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$repo = 'felichita/nanace'
$ref = if ($env:NANACE_REF) { $env:NANACE_REF } else { 'main' }
$skillUrl = if ($env:NANACE_BASE_URL) { $env:NANACE_BASE_URL } else {
  "https://raw.githubusercontent.com/$repo/$ref/integrations/claude-code/skills/nanace"
}
$configDir = if ($env:CLAUDE_CONFIG_DIR) { $env:CLAUDE_CONFIG_DIR } else { Join-Path $HOME '.claude' }
$skillDir = Join-Path (Join-Path $configDir 'skills') 'nanace'
$skillFiles = @('SKILL.md', 'schema-template.md')
$binDir = if ($env:NANACE_BIN_DIR) { $env:NANACE_BIN_DIR } else { Join-Path $env:LOCALAPPDATA 'nanace\bin' }
$bin = Join-Path $binDir 'nanace-mcp.exe'
# Records which release the installed binary came from and its checksum.
$marker = Join-Path $binDir '.nanace-mcp.version'
$skipBinary = $env:NANACE_SKIP_BINARY -eq '1'
$apiUrl = $(if ($env:NANACE_API_URL) { $env:NANACE_API_URL } else { 'http://127.0.0.1:8080' }).TrimEnd('/')
$tokenFile = if ($env:NANACE_TOKEN_FILE) { $env:NANACE_TOKEN_FILE } else { Join-Path $env:LOCALAPPDATA 'nanace\claude-code.token' }

function Get-Sha256([string]$path) { (Get-FileHash -Algorithm SHA256 $path).Hash.ToLowerInvariant() }
function Save-Url([string]$url, [string]$out) { Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $out }

function Install-NanaceMcp([string]$tmp) {
  $arch = $env:PROCESSOR_ARCHITECTURE
  if ($arch -ne 'AMD64' -and $arch -ne 'ARM64') {
    Write-Warning "no nanace-mcp release build for $arch; skipping the binary"
    return
  }
  # Only an x64 build is published; Windows on ARM runs it under emulation.
  $target = 'x86_64-pc-windows-msvc'

  $tag = $env:NANACE_VERSION
  if (-not $tag) {
    try {
      $tag = (Invoke-RestMethod -UseBasicParsing "https://api.github.com/repos/$repo/releases/latest").tag_name
    } catch { throw "could not find the latest nanace release: $($_.Exception.Message)" }
  }
  if (-not $tag.StartsWith('v')) { $tag = "v$tag" }

  $markerTag = ''; $markerSum = ''
  if (Test-Path $marker) { $markerTag, $markerSum = (Get-Content $marker -TotalCount 1).Split(' ') }
  if ((Test-Path $bin) -and $markerTag -eq $tag -and (Get-Sha256 $bin) -eq $markerSum) {
    Write-Host "nanace-mcp $tag is up to date: $bin"
    return
  }

  $name = "nanace-$($tag.Substring(1))-$target"
  $archive = Join-Path $tmp "$name.zip"
  Write-Host "Downloading nanace-mcp $tag ($target)..."
  try {
    Save-Url "https://github.com/$repo/releases/download/$tag/$name.zip" $archive
    Save-Url "https://github.com/$repo/releases/download/$tag/$name.zip.sha256" "$archive.sha256"
  } catch { throw "could not download $name.zip from release ${tag}: $($_.Exception.Message)" }
  $want = ((Get-Content "$archive.sha256" -TotalCount 1).Trim() -split '\s+')[0].ToLowerInvariant()
  if ((Get-Sha256 $archive) -ne $want) { throw "checksum mismatch for $name.zip" }
  Expand-Archive -Path $archive -DestinationPath (Join-Path $tmp 'x') -Force
  $new = Join-Path $tmp "x\$name\nanace-mcp.exe"
  if (-not (Test-Path $new)) { throw "nanace-mcp.exe not found in $name.zip" }

  $have = Get-Sha256 $new
  New-Item -ItemType Directory -Path $binDir -Force | Out-Null
  if ((Test-Path $bin) -and (Get-Sha256 $bin) -eq $have) {
    Write-Host "nanace-mcp $tag is already installed: $bin"
  } else {
    $existed = Test-Path $bin
    if ($existed) {
      # A running .exe cannot be overwritten, but it can be renamed.
      $old = "$bin.old"
      Remove-Item $old -Force -ErrorAction SilentlyContinue
      Move-Item $bin $old -Force
    }
    Copy-Item $new $bin
    if ($existed) { Remove-Item "$bin.old" -Force -ErrorAction SilentlyContinue }
    if ($markerTag -eq $tag) { Write-Host "Reinstalled nanace-mcp $tag (the installed file did not match): $bin" }
    elseif ($markerTag) { Write-Host "Updated nanace-mcp $markerTag -> ${tag}: $bin" }
    elseif ($existed) { Write-Host "Replaced an unmanaged nanace-mcp with ${tag}: $bin" }
    else { Write-Host "Installed nanace-mcp ${tag}: $bin" }
  }
  Set-Content -Path $marker -Value "$tag $have" -Encoding ascii
}

function Install-NanaceSkill([string]$tmp) {
  Write-Host "Downloading the /nanace skill ($ref)..."
  $dir = Join-Path $tmp 'skill'
  New-Item -ItemType Directory -Path $dir | Out-Null
  foreach ($f in $skillFiles) {
    $out = Join-Path $dir $f
    try { Save-Url "$skillUrl/$f" $out } catch { throw "could not download $skillUrl/$f : $($_.Exception.Message)" }
    if ((Get-Item $out).Length -eq 0) { throw "$f is empty" }
  }
  if ((Get-Content (Join-Path $dir 'SKILL.md') -TotalCount 1) -ne '---') { throw 'SKILL.md does not look like a skill file' }

  if (Test-Path $skillDir) {
    $changed = $false
    foreach ($f in $skillFiles) {
      $cur = Join-Path $skillDir $f
      if (-not (Test-Path $cur) -or (Get-Sha256 $cur) -ne (Get-Sha256 (Join-Path $dir $f))) { $changed = $true }
    }
    if (-not $changed) { Write-Host "/nanace skill is up to date: $skillDir"; return }
    # Outside skills\, or Claude Code would load the backup as a second skill.
    $backupDir = Join-Path $configDir 'skill-backups'
    New-Item -ItemType Directory -Path $backupDir -Force | Out-Null
    $backup = Join-Path $backupDir ('nanace-' + (Get-Date -Format 'yyyyMMddHHmmss'))
    Move-Item $skillDir $backup
    Write-Host "Previous skill moved to $backup"
  }
  New-Item -ItemType Directory -Path $skillDir -Force | Out-Null
  foreach ($f in $skillFiles) { Copy-Item (Join-Path $dir $f) (Join-Path $skillDir $f) }
  Write-Host "Installed the /nanace skill: $skillDir"
}

function Ask([string]$question, [bool]$defaultYes) {
  $hint = if ($defaultYes) { '[Y/n]' } else { '[y/N]' }
  $answer = Read-Host "$question $hint"
  if (-not $answer) { return $defaultYes }
  return $answer -match '^(y|yes)$'
}

function Get-RegisteredCommand {
  # Windows PowerShell 5.1 turns native stderr into a terminating error under 'Stop'.
  $line = $null
  try {
    $ErrorActionPreference = 'Continue'
    $line = (& claude mcp get nanace 2>$null) | Where-Object { $_ -match '^\s*Command:\s*(.+)$' } | Select-Object -First 1
  } catch { $line = $null } finally { $ErrorActionPreference = 'Stop' }
  if ($line -match '^\s*Command:\s*(.+)$') { return $Matches[1].Trim() }
  return $null
}

# API GET with the token from $tokenFile; in-process, so the token is never a process argument.
function Invoke-NanaceApi([string]$path) {
  $token = (Get-Content -Raw $tokenFile).Trim()
  Invoke-RestMethod -UseBasicParsing -TimeoutSec 20 -Uri "$apiUrl$path" -Headers @{ Authorization = "Bearer $token" }
}

# Readable and writable by the current user only.
function Protect-TokenFile([string]$path) {
  & icacls $path /inheritance:r /grant:r "$($env:USERNAME):(R,W)" | Out-Null
}

function Read-NanaceToken {
  Write-Host 'Create an access token for Claude Code:'
  Write-Host "  1. Open $apiUrl/auth/complete and sign in."
  Write-Host "  2. Create a token: scope 'write' (needed to save pages), any name, up to 365 days."
  Write-Host '  3. Copy it and paste it here (input is hidden).'
  try { Start-Process "$apiUrl/auth/complete" } catch { }
  New-Item -ItemType Directory -Path (Split-Path $tokenFile) -Force | Out-Null
  for ($i = 0; $i -lt 3; $i++) {
    $secure = Read-Host 'Token' -AsSecureString
    $ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
    try { $token = ([Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr)).Trim() }
    finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr) }
    if (-not $token.StartsWith('nnc_pat_')) {
      Write-Warning 'that does not look like a nanace token (it starts with nnc_pat_)'
      continue
    }
    Set-Content -Path $tokenFile -Value $token -Encoding ascii -NoNewline
    Protect-TokenFile $tokenFile
    $token = $null
    try { return Invoke-NanaceApi '/api/v1/me' } catch {
      Write-Warning 'the server rejected this token (invalid, expired or revoked); try again'
    }
  }
  Remove-Item $tokenFile -Force -ErrorAction SilentlyContinue
  return $null
}

function Get-TokenScope {
  # The API shows the first 12 characters (`nnc_pat_` + 4) of each token.
  $saved = (Get-Content -Raw $tokenFile).Trim()
  if ($saved.Length -lt 12) { return $null }
  $prefix = $saved.Substring(0, 12)
  try { $tokens = @(Invoke-NanaceApi '/api/v1/me/tokens') } catch { return $null }
  $scopes = @($tokens | Where-Object { $_.token_prefix -eq $prefix -and -not $_.revoked_at } | ForEach-Object { $_.scope } | Sort-Object -Unique)
  if ($scopes.Count -eq 1) { return $scopes[0] }
  return $null
}

function Select-Org($me) {
  if ($env:NANACE_ORG_ID) { return $env:NANACE_ORG_ID }
  $orgs = @($me.memberships | Sort-Object org_slug -Unique | Select-Object org_id, org_slug)
  if ($orgs.Count -eq 0) {
    Write-Warning 'your account is not a member of any organization yet: ask an org admin to add you'
    Write-Warning '(and to a team as writer to save pages); the connection will work once you are added'
    return $null
  }
  if ($orgs.Count -eq 1) { return $orgs[0].org_id }
  Write-Host 'You belong to several organizations:'
  for ($i = 0; $i -lt $orgs.Count; $i++) { Write-Host "  $($i + 1)) $($orgs[$i].org_slug)" }
  $pick = Read-Host 'Default organization for Claude [1]'
  $n = 1
  if (-not [int]::TryParse($pick, [ref]$n) -or $n -lt 1 -or $n -gt $orgs.Count) { $n = 1 }
  return $orgs[$n - 1].org_id
}

function Show-ManualConnect {
  Write-Host 'To connect later: run this installer again in a terminal, or set it up by hand:'
  Write-Host "  1. Create a 'write' token at $apiUrl/auth/complete and save it to $tokenFile."
  Write-Host "  2. claude mcp add nanace --scope user --env NANACE_API_URL=$apiUrl ``"
  Write-Host "       --env `"NANACE_TOKEN_FILE=$tokenFile`" --env NANACE_ALLOW_WRITES=true -- `"$bin`""
  Write-Host "Or use 'Connect to Claude' in the nanace desktop app (it registers its own nanace-mcp)."
}

function Connect-Claude {
  Write-Host ''
  if (-not (Get-Command claude -ErrorAction SilentlyContinue)) {
    Write-Host 'Claude Code CLI not found; install it and run this installer again to connect.'
    return
  }
  if (-not (Test-Path $bin)) {
    Write-Host 'nanace-mcp is not installed here, so Claude Code cannot be connected to it.'
    return
  }
  $cur = Get-RegisteredCommand
  if ($cur -eq $bin -and $env:NANACE_RECONNECT -ne '1') {
    Write-Host "Claude Code is connected through this nanace-mcp: $bin"
    Write-Host '(NANACE_RECONNECT=1 connects again, e.g. with a new token or server.)'
    return
  }
  if ($cur -and $cur -ne $bin) {
    Write-Host "Claude Code currently runs a different nanace-mcp: $cur"
    Write-Host '(that copy is not managed by this installer; the desktop app updates its own)'
  }
  if ($env:NANACE_CONNECT -eq '0') { Show-ManualConnect; return }
  if ([Console]::IsInputRedirected) { Write-Host 'No terminal to ask for a token.'; Show-ManualConnect; return }
  if ($cur -eq $bin) {
    if (-not (Ask "Connect Claude Code again to the nanace server at ${apiUrl}?" $true)) { return }
  } elseif ($cur) {
    if (-not (Ask "Switch Claude Code to $bin and ${apiUrl}?" $false)) { return }
  } elseif (-not (Ask "Connect Claude Code to the nanace server at ${apiUrl}?" $true)) {
    Show-ManualConnect; return
  }

  try { Invoke-WebRequest -UseBasicParsing -TimeoutSec 10 -Uri "$apiUrl/livez" | Out-Null } catch {
    Write-Warning "cannot reach $apiUrl (set NANACE_API_URL to your nanace server)"
    Show-ManualConnect; return
  }

  $me = $null
  if ((Test-Path $tokenFile) -and (Get-Item $tokenFile).Length -gt 0) {
    try { $me = Invoke-NanaceApi '/api/v1/me' } catch { $me = $null }
    if ($me -and -not (Ask "Use the token saved in ${tokenFile}?" $true)) { $me = $null }
  }
  if (-not $me) { $me = Read-NanaceToken }
  if (-not $me) { Write-Warning 'no valid token; Claude Code was not connected'; Show-ManualConnect; return }
  Protect-TokenFile $tokenFile

  $scope = Get-TokenScope
  if ($scope -eq 'read') { Write-Warning "this token has 'read' scope: Claude can search but /nanace cannot save pages" }
  if (-not ($me.memberships | Where-Object { $_.team_role -in @('writer', 'admin') })) {
    Write-Warning 'you are not a writer in any team yet: /nanace can only save after a team admin adds you'
  }
  $org = Select-Org $me

  if ($cur) { try { $ErrorActionPreference = 'Continue'; & claude mcp remove nanace --scope user 2>$null | Out-Null } finally { $ErrorActionPreference = 'Stop' } }
  $mcpArgs = @('mcp', 'add', 'nanace', '--scope', 'user', '--env', "NANACE_API_URL=$apiUrl", '--env', "NANACE_TOKEN_FILE=$tokenFile")
  if ($scope -ne 'read') { $mcpArgs += @('--env', 'NANACE_ALLOW_WRITES=true') }
  if ($org) { $mcpArgs += @('--env', "NANACE_ORG_ID=$org") }
  $mcpArgs += @('--', $bin)
  $code = 1
  try { $ErrorActionPreference = 'Continue'; & claude @mcpArgs 2>&1 | Out-Null; $code = $LASTEXITCODE }
  finally { $ErrorActionPreference = 'Stop' }
  if ($code -ne 0) { Write-Warning 'claude mcp add failed'; Show-ManualConnect; return }
  Write-Host "Connected Claude Code to $apiUrl as $($me.email) (token file: $tokenFile)."
  if ((Get-RegisteredCommand) -eq $bin) { Write-Host 'Claude Code has the nanace server registered.' }
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ('nanace-install-' + [Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  if ($skipBinary) { Write-Host 'Skipping nanace-mcp (NANACE_SKIP_BINARY=1).' } else { Install-NanaceMcp $tmp }
  Install-NanaceSkill $tmp
  if (-not $skipBinary) { Connect-Claude }
  Write-Host ''
  Write-Host 'Restart Claude Code to pick up changes, then run: /nanace <TASK-KEY>'
} finally {
  Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
