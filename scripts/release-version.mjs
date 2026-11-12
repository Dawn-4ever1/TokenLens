#!/usr/bin/env node
// One version for everything a release ships: the desktop app and the
// collector it embeds (D043).
//
//   node scripts/release-version.mjs set 0.2.0     write it into every file below
//   node scripts/release-version.mjs check v0.2.0  fail unless every file has it
//
// The release workflow runs `check` with the pushed tag, so a tag that does
// not match the files never produces installers.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const SEMVER = /^\d+\.\d+\.\d+$/;

// [file, how to find its version]. Each pattern captures the version in group 2.
const FIELDS = [
  ["app/src-tauri/tauri.conf.json", /^(\s*"version": ")([^"]+)(")/m],
  ["app/package.json", /^(\s*"version": ")([^"]+)(")/m],
  // package-lock.json: the top-level entry, then the root package entry.
  ["app/package-lock.json", /^(  "version": ")([^"]+)(")/m],
  ["app/package-lock.json", /^(    "": \{\n      "name": "tokenlens-app",\n      "version": ")([^"]+)(")/m],
  ["app/src-tauri/Cargo.toml", /^(\[package\]\nname = "tokenlens-app"\nversion = ")([^"]+)(")/m],
  ["crates/collector-core/Cargo.toml", /^(\[package\]\nname = "collector-core"\nversion = ")([^"]+)(")/m],
  ["collector/Cargo.toml", /^(\[package\]\nname = "collector"\nversion = ")([^"]+)(")/m],
  ["Cargo.lock", /^(name = "tokenlens-app"\nversion = ")([^"]+)(")/m],
  ["Cargo.lock", /^(name = "collector-core"\nversion = ")([^"]+)(")/m],
  ["Cargo.lock", /^(name = "collector"\nversion = ")([^"]+)(")/m],
];

function read(file) {
  // Git may check files out with CRLF on Windows; match on LF, write back as found.
  const raw = readFileSync(join(root, file), "utf8");
  return { text: raw.replace(/\r\n/g, "\n"), crlf: raw.includes("\r\n") };
}

function versions() {
  return FIELDS.map(([file, pattern]) => {
    const match = read(file).text.match(pattern);
    if (!match) throw new Error(`${file}: version field not found (${pattern})`);
    return { file, version: match[2] };
  });
}

const [command, value] = process.argv.slice(2);
const wanted = (value ?? "").replace(/^v/, "");
if (!["set", "check"].includes(command) || !SEMVER.test(wanted)) {
  console.error("usage: node scripts/release-version.mjs set|check <x.y.z or vx.y.z>");
  process.exit(2);
}

if (command === "set") {
  for (const [file, pattern] of FIELDS) {
    const { text, crlf } = read(file);
    const next = text.replace(pattern, (_, before, _old, after) => `${before}${wanted}${after}`);
    writeFileSync(join(root, file), crlf ? next.replace(/\n/g, "\r\n") : next);
  }
}

const wrong = versions().filter((entry) => entry.version !== wanted);
if (wrong.length) {
  for (const entry of wrong) console.error(`${entry.file}: ${entry.version}, expected ${wanted}`);
  process.exit(1);
}
console.log(`all ${FIELDS.length} version fields are ${wanted}`);
