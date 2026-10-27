## Downloads

**Desktop app**

| Platform | File |
|---|---|
| macOS, Apple silicon | `nanace_<version>_aarch64.dmg` |
| macOS, Intel | `nanace_<version>_x64.dmg` |
| Windows x64 | `nanace_<version>_x64-setup.exe` or `.msi` |
| Linux x64 / arm64 | `.AppImage`, `.deb` or `.rpm` |

**Server + MCP** (`nanace-server`, `nanace-mcp`): `nanace-<version>-<target>.tar.gz` (`.zip` on Windows). Each archive has a `.sha256` checksum. Linux server builds need glibc 2.38+ (Ubuntu 24.04, Debian 13, Fedora 39 or newer).

The builds are not code-signed yet:

- macOS: if the app "is damaged" or cannot be opened, run `xattr -cr /Applications/nanace.app` once.
- Windows: SmartScreen may warn; choose "More info" → "Run anyway".

The server needs PostgreSQL and Qdrant; see the README for setup.
