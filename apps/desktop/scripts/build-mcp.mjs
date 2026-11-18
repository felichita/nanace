// Builds the sidecars (nanace-mcp and the P2P daemon nanace-p2p) and places
// them where Tauri expects them:
// src-tauri/binaries/<name>-<target triple>[.exe].
//
// The Tauri CLI sets TAURI_ENV_TARGET_TRIPLE for its before* commands, so
// `tauri build --target <triple>` gets a sidecar for that target.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const app = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const root = resolve(app, "../..");
const host = /^host: (\S+)$/m.exec(execFileSync("rustc", ["-vV"], { encoding: "utf8" }))[1];
const target = process.env.TAURI_ENV_TARGET_TRIPLE || host;
const ext = target.includes("windows") ? ".exe" : "";

const dest = join(app, "src-tauri", "binaries");
mkdirSync(dest, { recursive: true });
// Built one at a time: together, Cargo would give nanace-mcp the daemon
// features of nanace-p2p and with them libp2p.
for (const name of ["nanace-mcp", "nanace-p2p"]) {
  const args = ["build", "--release", "--locked", "-p", name, "--manifest-path", join(root, "Cargo.toml")];
  if (target !== host) args.push("--target", target);
  execFileSync("cargo", args, { stdio: "inherit" });
  const built = join(root, "target", ...(target !== host ? [target] : []), "release", `${name}${ext}`);
  copyFileSync(built, join(dest, `${name}-${target}${ext}`));
  console.log(`sidecar: ${name}-${target}${ext}`);
}
