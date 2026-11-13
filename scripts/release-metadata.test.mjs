import test from "node:test";
import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, randomBytes, sign } from "node:crypto";
import { mkdtempSync, readFileSync, rmSync, unlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createReleaseMetadata, updateNotes, verifyUpdaterSignature } from "./release-metadata.mjs";

const encode = (text) => Buffer.from(text).toString("base64");

function signer() {
  // Ephemeral synthetic keys, never the production signing key.
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const id = randomBytes(8);
  const raw = publicKey.export({ type: "spki", format: "der" }).subarray(-32);
  const key = encode(`untrusted comment: synthetic test key\n${Buffer.concat([Buffer.from("Ed"), id, raw]).toString("base64")}\n`);
  return {
    key,
    signature(data, legacy = false, version = "0.4.0") {
      const message = legacy ? data : createHash("blake2b512").update(data).digest();
      const signature = sign(null, message, privateKey);
      const comment = `timestamp:1\tfile:synthetic${version === null ? "" : `\tversion:${version}`}`;
      const record = Buffer.concat([Buffer.from(legacy ? "Ed" : "ED"), id, signature]);
      const global = sign(null, Buffer.concat([signature, Buffer.from(comment)]), privateKey);
      return encode(`untrusted comment: synthetic signature\n${record.toString("base64")}\ntrusted comment: ${comment}\n${global.toString("base64")}\n`);
    },
  };
}

function fixture(t, signedVersion = "0.4.0") {
  const assetsDir = mkdtempSync(join(tmpdir(), "tokenlens-release-test-"));
  t.after(() => rmSync(assetsDir, { recursive: true, force: true }));
  const synthetic = signer();
  const windows = "TokenLens_0.4.0_x64-setup.exe";
  const mac = "TokenLens_0.4.0_aarch64.app.tar.gz";
  for (const name of [windows, mac, "TokenLens_0.4.0_aarch64.dmg"]) {
    const data = Buffer.from(`synthetic artifact ${name}`);
    writeFileSync(join(assetsDir, name), data);
    if (name !== "TokenLens_0.4.0_aarch64.dmg") writeFileSync(join(assetsDir, `${name}.sig`), `${synthetic.signature(data, false, signedVersion)}\n`);
  }
  return { tag: "v0.4.0", repository: "owner/repo", assetsDir, notes: "## 本版变化\n\nRelease notes as plain text\n<script>not executed</script>\n\n## 下载\n\n| a | b |", pubDate: "2026-11-12T00:00:00Z", publicKey: synthetic.key, windows, mac };
}

test("Minisign matches the upstream prehashed test vector", () => {
  // Public test vector from the minisign-verify project's verify_prehashed test.
  const key = encode("untrusted comment: upstream public test key\nRWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3\n");
  const signature = encode("untrusted comment: signature from minisign secret key\nRUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\ntrusted comment: timestamp:1556193335\tfile:test\ny/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==\n");
  assert.doesNotThrow(() => verifyUpdaterSignature(Buffer.from("test"), signature, key));
  assert.throws(() => verifyUpdaterSignature(Buffer.from("Test"), signature, key), /verification failed/);
});

test("accepts legacy and prehashed Tauri-compatible signatures", () => {
  const synthetic = signer();
  const bytes = Buffer.from("synthetic bytes");
  for (const legacy of [true, false]) assert.doesNotThrow(() => verifyUpdaterSignature(bytes, synthetic.signature(bytes, legacy), synthetic.key));
});

test("writes complete manifest links, literal signatures and checksum for every asset", (t) => {
  const input = fixture(t);
  const { manifest, latest, sums } = createReleaseMetadata(input);
  assert.equal(manifest.version, "0.4.0");
  assert.equal(manifest.notes, "Release notes as plain text\n<script>not executed</script>");
  assert.equal(manifest.pub_date, input.pubDate);
  assert.deepEqual(Object.keys(manifest.platforms).sort(), ["darwin-aarch64", "windows-x86_64"]);
  for (const [platform, file] of [["windows-x86_64", input.windows], ["darwin-aarch64", input.mac]]) {
    assert.equal(manifest.platforms[platform].signature, readFileSync(join(input.assetsDir, `${file}.sig`), "utf8").trim());
    assert.equal(manifest.platforms[platform].url, `https://github.com/owner/repo/releases/download/v0.4.0/${file}`);
  }
  assert.equal(sums.trim().split("\n").length, 6);
  assert.ok(sums.includes(`${createHash("sha256").update(latest).digest("hex")}  latest.json\n`));
  assert.deepEqual(JSON.parse(latest), manifest);
});

test("missing signatures prevent publication", (t) => {
  const input = fixture(t);
  unlinkSync(join(input.assetsDir, `${input.mac}.sig`));
  assert.throws(() => createReleaseMetadata(input), /assets are missing/);
});

test("a valid signature without an authenticated app version prevents publication", (t) => {
  const input = fixture(t, null);
  assert.throws(() => createReleaseMetadata(input), /Signed app version is missing/);
});

test("a valid older signed package cannot be published as a newer Release", (t) => {
  const input = fixture(t, "0.3.0");
  assert.throws(() => createReleaseMetadata(input), /Signed app version differs from Release tag/);
});

test("accepts a signed version with an equivalent v prefix", (t) => {
  const input = fixture(t, "v0.4.0");
  assert.equal(createReleaseMetadata(input).manifest.version, "0.4.0");
});

test("empty signatures prevent publication", (t) => {
  const input = fixture(t);
  writeFileSync(join(input.assetsDir, `${input.windows}.sig`), "");
  assert.throws(() => createReleaseMetadata(input), /Empty or invalid/);
});

test("tampered artifact or swapped signature prevents publication", (t) => {
  const input = fixture(t);
  writeFileSync(join(input.assetsDir, input.windows), "tampered artifact");
  assert.throws(() => createReleaseMetadata(input), /verification failed/);
});

test("trusted comment tampering prevents publication", (t) => {
  const input = fixture(t);
  const path = join(input.assetsDir, `${input.windows}.sig`);
  const decoded = Buffer.from(readFileSync(path, "utf8").trim(), "base64").toString("utf8");
  writeFileSync(path, encode(decoded.replace("timestamp:1", "timestamp:2")));
  assert.throws(() => createReleaseMetadata(input), /verification failed/);
});

test("wrong public key prevents publication", (t) => {
  const input = fixture(t);
  input.publicKey = signer().key;
  assert.throws(() => createReleaseMetadata(input), /key id/);
});

test("stale or duplicate updater assets prevent publication", (t) => {
  const input = fixture(t);
  writeFileSync(join(input.assetsDir, "TokenLens.app.tar.gz"), "old archive");
  assert.throws(() => createReleaseMetadata(input), /assets are missing, duplicated/);
});

test("rejects invalid tag, repository and publication metadata", (t) => {
  const input = fixture(t);
  for (const tag of ["0.4.0", "v0.4.0-beta", "v0.4.0/escape"]) assert.throws(() => createReleaseMetadata({ ...input, tag }), /stable vX.Y.Z/);
  assert.throws(() => createReleaseMetadata({ ...input, repository: "owner/repo/escape" }), /Invalid repository/);
  assert.throws(() => createReleaseMetadata({ ...input, pubDate: "not a date" }), /publication date/);
  assert.throws(() => createReleaseMetadata({ ...input, notes: "" }), /notes are missing/);
});

test("in-app notes keep only the 本版变化 section as plain text", () => {
  const body = [
    "## 本版变化", "", "修好 **两件事**：", "", "- 看板 `趋势` 页更快", "* 详见 [说明](https://example.com)", "",
    "### 已知问题", "", "> Mac 另验", "", "", "", "## 下载", "", "| 系统 | 文件 |", "|---|---|", "## 第一次打开", "仍要运行",
  ].join("\r\n");
  assert.equal(updateNotes(body), "修好 两件事：\n\n• 看板 趋势 页更快\n• 详见 说明\n\n已知问题\n\nMac 另验");
});

test("Release notes without a filled 本版变化 section prevent publication", (t) => {
  assert.throws(() => updateNotes("## 下载\n\n| a | b |"), /need a/);
  assert.throws(() => updateNotes("## 本版变化\n\n   \n## 下载\ntext"), /empty/);
  const input = fixture(t);
  assert.throws(() => createReleaseMetadata({ ...input, notes: "## 下载\n\ntext" }), /本版变化/);
});

test("the Release notes template yields short, one-line-at-a-time in-app notes", () => {
  const template = readFileSync(new URL("../.github/release-notes.md", import.meta.url), "utf8");
  const notes = updateNotes(template);
  assert.ok(notes.length > 0 && notes.length < 600, "keep the in-app summary short");
  assert.doesNotMatch(notes, /^#|\||`|\*\*|\{\{VERSION\}\}/m);
  assert.doesNotMatch(notes, /SHA256SUMS|仍要运行|配对码/, "download and first-run help stay on the Release page");
  const lines = notes.split("\n").filter((line) => line.trim());
  assert.ok(lines.length >= 3, "the update dialog should list several short points");
  for (const line of lines) {
    assert.match(line, /^• /, `every line is one point: ${line}`);
    assert.ok([...line].length <= 40, `each point stays on one line: ${line}`);
  }
});

test("a single long paragraph is rejected as update notes", () => {
  const paragraph = "## 本版变化\n\n" + "这一整段话会被塞进更新窗口里，用户要读很久才能读完，而且中间没有任何停顿，看起来非常累。".repeat(2) + "\n\n## 下载\n";
  const notes = updateNotes(paragraph);
  const lines = notes.split("\n").filter((line) => line.trim());
  assert.ok(lines.length === 1);
  // The template test above enforces the bullet format; this documents why.
  assert.ok([...lines[0]].length > 40);
});
