#!/usr/bin/env node
// Keep build-machine paths out of release binaries. The public repository name
// is intentionally embedded in the updater, not a private machine identifier.
import { readFileSync } from "node:fs";
import { homedir, userInfo } from "node:os";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const PUBLIC_REPOSITORY = "Dawn-4ever1/TokenLens";
const PUBLIC_HOST = "https://github.com/";
const PUBLIC_UPDATE_PATHS = ["/releases/latest/download/latest.json", "/releases/download/"];

function occurrences(bytes, needle) {
  const positions = [];
  for (let start = 0; start <= bytes.length - needle.length;) {
    const position = bytes.indexOf(needle, start);
    if (position < 0) break;
    positions.push(position);
    start = position + 1;
  }
  return positions;
}

function asciiFold(bytes) {
  return bytes.map((byte) => byte >= 65 && byte <= 90 ? byte + 32 : byte);
}

function approvedRepositoryRanges(bytes, encoding) {
  const marker = Buffer.from(PUBLIC_REPOSITORY, encoding);
  const host = Buffer.from(PUBLIC_HOST, encoding);
  const width = encoding === "utf16le" ? 2 : 1;
  const character = (position) => bytes.subarray(Math.max(0, position), Math.max(0, position + width)).toString(encoding);
  const delimiter = (char) => !/[A-Za-z0-9_./\\:-]/.test(char);
  return occurrences(bytes, marker).filter((start) => {
    const end = start + marker.length;
    const inGitHubUrl = start >= host.length && bytes.subarray(start - host.length, start).equals(host);
    if (inGitHubUrl) {
      return PUBLIC_UPDATE_PATHS.some((path) => bytes.subarray(end, end + Buffer.byteLength(path, encoding)).equals(Buffer.from(path, encoding)))
        || end === bytes.length || delimiter(character(end));
    }
    // A standalone identifier is allowed; a local path, another owner's URL,
    // prefixed identifier, or private repository suffix is not.
    return (start === 0 || delimiter(character(start - width))) && (end === bytes.length || delimiter(character(end)));
  }).map((start) => [start, start + marker.length]);
}

export function scanBundlePrivacy(bytes, { home, username, checkUsername = true }) {
  if (!home || (checkUsername && !username)) throw new Error("Build privacy markers are unavailable");
  const leaks = [];
  const folded = asciiFold(bytes);
  for (const encoding of ["utf8", "utf16le"]) {
    const label = encoding === "utf8" ? "UTF-8" : "UTF-16LE";
    // Never exempt a home path, even if it happens to contain a public name.
    const homeForms = new Set([home, home.replaceAll("\\", "/"), home.replaceAll("/", "\\")]);
    if ([...homeForms].some((path) => folded.includes(asciiFold(Buffer.from(path, encoding))))) leaks.push(`home path (${label})`);
    if (checkUsername) {
      const needle = asciiFold(Buffer.from(username, encoding));
      const approved = approvedRepositoryRanges(bytes, encoding);
      const privateMatch = occurrences(folded, needle).some((start) => !approved.some(([from, to]) => start >= from && start + needle.length <= to));
      if (privateMatch) leaks.push(`user name (${label})`);
    }
  }
  return leaks;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [binary, mode] = process.argv.slice(2);
    if (!binary || (mode && mode !== "--home-only")) throw new Error("usage: node scripts/check-bundle-privacy.mjs <binary> [--home-only]");
    const leaks = scanBundlePrivacy(readFileSync(binary), {
      home: homedir(), username: process.env.USERNAME || process.env.USER || userInfo().username,
      checkUsername: mode !== "--home-only",
    });
    if (leaks.length) throw new Error(`The app binary still contains private build markers: ${leaks.join(", ")}`);
    console.log("Build privacy check passed (only the exact public repository identifier is exempt from user-name checks).");
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
