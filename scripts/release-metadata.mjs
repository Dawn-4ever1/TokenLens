#!/usr/bin/env node
// Validate both signed updater bundles before publishing a complete Release.
// Tauri's .sig and pubkey are base64-encoded Minisign text, not URLs or paths.
// Format: https://jedisct1.github.io/minisign/ and tauri-plugin-updater.
import { createHash, createPublicKey, verify } from "node:crypto";
import { readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

function base64(value, label) {
  const text = value.trim();
  if (!text || text.length % 4 !== 0 || !/^[A-Za-z0-9+/]+={0,2}$/.test(text)) {
    throw new Error(`Invalid ${label} encoding`);
  }
  const bytes = Buffer.from(text, "base64");
  if (bytes.toString("base64") !== text) throw new Error(`Invalid ${label} encoding`);
  return bytes;
}

// Minisign binds the Ed25519 file signature to a trusted comment with a second
// Ed25519 signature. Current Tauri signatures use a BLAKE2b-512 file digest.
// Verify both, plus the embedded key id; no private material is ever needed.
export function verifyUpdaterSignature(content, signature, publicKey) {
  const publicLines = base64(publicKey, "public key").toString("utf8").split(/\r?\n/);
  if (!publicLines[0]?.startsWith("untrusted comment: ")) throw new Error("Invalid public key format");
  const key = base64(publicLines[1] ?? "", "public key record");
  if (key.length !== 42 || !["Ed", "ED"].includes(key.subarray(0, 2).toString("ascii"))) {
    throw new Error("Invalid public key record");
  }
  const lines = base64(signature, "signature").toString("utf8").split(/\r?\n/);
  if (!lines[0]?.startsWith("untrusted comment: ") || !lines[2]?.startsWith("trusted comment: ") || lines.slice(4).some(Boolean)) {
    throw new Error("Invalid Minisign format");
  }
  const record = base64(lines[1] ?? "", "signature record");
  const globalSignature = base64(lines[3] ?? "", "trusted comment signature");
  if (record.length !== 74 || globalSignature.length !== 64 || !record.subarray(2, 10).equals(key.subarray(2, 10))) {
    throw new Error("Signature key id or length mismatch");
  }
  const algorithm = record.subarray(0, 2).toString("ascii");
  if (!["Ed", "ED"].includes(algorithm)) throw new Error("Unsupported signature algorithm");
  const fileSignature = record.subarray(10);
  const message = algorithm === "ED" ? createHash("blake2b512").update(content).digest() : content;
  const ed25519 = createPublicKey({ key: Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), key.subarray(10)]), format: "der", type: "spki" });
  const trustedComment = lines[2].slice("trusted comment: ".length);
  const trusted = Buffer.concat([fileSignature, Buffer.from(trustedComment)]);
  if (!verify(null, message, ed25519, fileSignature) || !verify(null, trusted, ed25519, globalSignature)) {
    throw new Error("Updater signature verification failed");
  }
  return trustedComment;
}

// The in-app update dialog shows plain text. Keep only the 「本版变化」 section
// of the Release page and drop Markdown markers; download and first-run help
// stay on the Release page, where they are useful.
export const CHANGES_HEADING = "本版变化";

export function updateNotes(body) {
  const lines = body.replace(/\r\n?/g, "\n").split("\n");
  const heading = (line) => /^(#{1,6})\s+(.*?)\s*#*\s*$/.exec(line);
  const start = lines.findIndex((line) => heading(line)?.[2] === CHANGES_HEADING);
  if (start < 0) throw new Error(`Release notes need a 「## ${CHANGES_HEADING}」 section`);
  const level = heading(lines[start])[1].length;
  const section = [];
  for (const line of lines.slice(start + 1)) {
    const match = heading(line);
    if (match && match[1].length <= level) break;
    section.push(match ? match[2] : line);
  }
  const text = section.join("\n")
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/\*\*|__|`/g, "")
    .replace(/^([ \t]*)[-*+][ \t]+/gm, "$1• ")
    .replace(/^[ \t]*>[ \t]?/gm, "")
    .replace(/[ \t]+$/gm, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  if (!text) throw new Error(`The 「${CHANGES_HEADING}」 section of the Release notes is empty`);
  return text;
}

export function createReleaseMetadata({ tag, repository, assetsDir, notes, pubDate, publicKey }) {
  if (!/^v\d+\.\d+\.\d+$/.test(tag)) throw new Error("Expected a stable vX.Y.Z tag");
  if (!/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)) throw new Error("Invalid repository");
  if (typeof notes !== "string" || !notes.trim()) throw new Error("Release notes are missing");
  if (typeof pubDate !== "string" || !/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(?:\.\d+)?Z$/.test(pubDate) || Number.isNaN(Date.parse(pubDate))) {
    throw new Error("Invalid publication date");
  }
  const version = tag.slice(1);
  const windows = `TokenLens_${version}_x64-setup.exe`;
  const mac = `TokenLens_${version}_aarch64.app.tar.gz`;
  const expected = [windows, `${windows}.sig`, `TokenLens_${version}_aarch64.dmg`, mac, `${mac}.sig`].sort();
  const actual = readdirSync(assetsDir).filter((name) => /\.(exe|dmg|sig)$|\.app\.tar\.gz$/.test(name)).sort();
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error("Release assets are missing, duplicated, or have the wrong version");
  for (const name of expected) {
    const stat = statSync(join(assetsDir, name));
    if (!stat.isFile() || stat.size === 0) throw new Error(`Empty or invalid Release asset: ${name}`);
  }
  const platform = (file) => {
    const signature = readFileSync(join(assetsDir, `${file}.sig`), "utf8").trim();
    const trustedComment = verifyUpdaterSignature(readFileSync(join(assetsDir, file)), signature, publicKey);
    // Match updater.requireSignedVersion: do not publish a correctly signed
    // older package under a newer manifest version. New releases must use a
    // Tauri CLI that signs the app version in the authenticated comment.
    const versions = trustedComment.split("\t").filter((field) => field.startsWith("version:")).map((field) => field.slice("version:".length));
    if (versions.length !== 1 || !versions[0]) throw new Error(`Signed app version is missing or ambiguous: ${file}`);
    if (versions[0].replace(/^v/, "") !== version) throw new Error(`Signed app version differs from Release tag: ${file}`);
    return { signature, url: `https://github.com/${repository}/releases/download/${tag}/${file}` };
  };
  const manifest = {
    version,
    notes: updateNotes(notes),
    pub_date: pubDate,
    platforms: { "windows-x86_64": platform(windows), "darwin-aarch64": platform(mac) },
  };
  const latest = `${JSON.stringify(manifest, null, 2)}\n`;
  const sums = [...expected, "latest.json"].sort().map((name) => {
    const data = name === "latest.json" ? latest : readFileSync(join(assetsDir, name));
    return `${createHash("sha256").update(data).digest("hex")}  ${name}\n`;
  }).join("");
  return { manifest, latest, sums };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const [tag, repository, assetsDir, notesFile] = process.argv.slice(2);
    if (!tag || !repository || !assetsDir || !notesFile) throw new Error("usage: node scripts/release-metadata.mjs <tag> <owner/repo> <assets-dir> <notes-file>");
    const config = JSON.parse(readFileSync(new URL("../app/src-tauri/tauri.conf.json", import.meta.url), "utf8"));
    if (`v${config.version}` !== tag) throw new Error("Tag differs from the app version");
    const result = createReleaseMetadata({ tag, repository, assetsDir, notes: readFileSync(notesFile, "utf8"), pubDate: new Date().toISOString(), publicKey: config.plugins?.updater?.pubkey });
    writeFileSync(join(assetsDir, "latest.json"), result.latest);
    writeFileSync(join(assetsDir, "SHA256SUMS.txt"), result.sums);
    console.log(`Verified both updater signatures; generated latest.json and SHA256SUMS.txt for ${tag}`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
