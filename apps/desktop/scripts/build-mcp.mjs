// Builds nanace-mcp and places it where Tauri expects the sidecar:
// src-tauri/binaries/nanace-mcp-<target triple>[.exe].
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

const args = ["build", "--release", "--locked", "-p", "nanace-mcp", "--manifest-path", join(root, "Cargo.toml")];
if (target !== host) args.push("--target", target);
execFileSync("cargo", args, { stdio: "inherit" });

const built = join(root, "target", ...(target !== host ? [target] : []), "release", `nanace-mcp${ext}`);
const dest = join(app, "src-tauri", "binaries");
mkdirSync(dest, { recursive: true });
copyFileSync(built, join(dest, `nanace-mcp-${target}${ext}`));
console.log(`sidecar: nanace-mcp-${target}${ext}`);
