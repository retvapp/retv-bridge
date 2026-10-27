#!/usr/bin/env node
// Tauri's bundle.windows.signCommand for the two-pass Windows signing of .github/workflows/release.yml. It never signs
// anything itself: SignPath signs only GitHub artifacts, so the payload is taken out in one build and put back in the
// next one. Tauri calls it once per file it would sign, with the file's path last:
//   retv-bridge.exe  the app, after Tauri patched the bundle type into it (so what is captured is what NSIS packs)
//   *.dll            NSIS plugins, unpacked to a temp folder at install time: left alone
//   *-setup.exe      the installer itself: left alone (SignPath signs it in pass 2)
//   anything else    the NSIS uninstaller, from `!uninstfinalize` (a temp file of makensis)
//
//   node win-sign-hook.mjs capture <dir> <file>
//     pass 0: copies the app to <dir>/retv-bridge.exe and the uninstaller to <dir>/uninstall.exe, plus the
//     uninstaller's SHA-256 to <dir>/uninstall.exe.sha256; a second uninstaller is an error.
//   node win-sign-hook.mjs swap <signed-dir> <unsigned-uninstaller-sha256> <file>
//     pass 2: the app must already be <signed-dir>/retv-bridge.exe byte for byte; the uninstaller makensis just built
//     must be the very one captured in pass 0 (same SHA-256), and is replaced with <signed-dir>/uninstall.exe.
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { basename, join } from "node:path";

const APP = "retv-bridge.exe";
const UNINSTALLER = "uninstall.exe";

/** "app" | "plugin" | "installer" | "uninstaller" */
export function kindOf(file) {
  const name = basename(file.replace(/\\/g, "/")).toLowerCase();
  if (name === APP) return "app";
  if (name.endsWith(".dll")) return "plugin";
  if (name.endsWith("-setup.exe")) return "installer";
  return "uninstaller";
}

const sha256 = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");

function capture(dir, file) {
  mkdirSync(dir, { recursive: true });
  const kind = kindOf(file);
  if (kind === "app") {
    copyFileSync(file, join(dir, APP));
    return `captured the app (${sha256(file)})`;
  }
  if (kind === "uninstaller") {
    const out = join(dir, UNINSTALLER);
    if (existsSync(out)) throw new Error(`a second uninstaller (${file}): only one is expected`);
    const hash = sha256(file);
    copyFileSync(file, out);
    writeFileSync(`${out}.sha256`, `${hash}\n`);
    return `captured the uninstaller (${hash})`;
  }
  return `left ${kind} alone`;
}

function swap(signedDir, expected, file) {
  const kind = kindOf(file);
  if (kind === "app") {
    const signed = join(signedDir, APP);
    if (!readFileSync(file).equals(readFileSync(signed))) throw new Error(`${file} is not the signed ${signed}`);
    return "the app is the signed one";
  }
  if (kind === "uninstaller") {
    if (!/^[0-9a-f]{64}$/.test(expected)) throw new Error(`"${expected}" is not a SHA-256`);
    const got = sha256(file);
    if (got !== expected) {
      throw new Error(`this uninstaller (${got}) is not the one signed in pass 1 (${expected}): the two builds differ`);
    }
    copyFileSync(join(signedDir, UNINSTALLER), file);
    return "put the signed uninstaller in place";
  }
  return `left ${kind} alone`;
}

function main(argv) {
  const [mode, ...rest] = argv;
  if (mode === "capture" && rest.length === 2) return capture(rest[0], rest[1]);
  if (mode === "swap" && rest.length === 3) return swap(rest[0], rest[1].trim(), rest[2]);
  throw new Error("usage: win-sign-hook.mjs capture <dir> <file> | swap <signed-dir> <sha256> <file>");
}

try {
  console.log(`win-sign-hook: ${main(process.argv.slice(2))}`);
} catch (e) {
  console.error(`win-sign-hook: ${e instanceof Error ? e.message : String(e)}`);
  process.exit(1);
}
