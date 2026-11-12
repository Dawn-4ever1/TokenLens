import test from "node:test";
import assert from "node:assert/strict";
import { PUBLIC_REPOSITORY, scanBundlePrivacy } from "./check-bundle-privacy.mjs";

// Synthetic machine markers: these are not a real home directory or account.
const markers = { home: "C:\\Users\\FixturePerson", username: "FixturePerson" };
// Derive a collision from the public handle, not a real account/home fixture.
const collisionName = PUBLIC_REPOSITORY.split("-")[0];
const collision = { home: `/synthetic-homes/${collisionName}-fixture`, username: collisionName };
const endpoint = `https://github.com/${PUBLIC_REPOSITORY}/releases/latest/download/latest.json`;
const download = `https://github.com/${PUBLIC_REPOSITORY}/releases/download/`;

for (const encoding of ["utf8", "utf16le"]) {
  const bytes = (text) => Buffer.from(text, encoding);
  test(`${encoding}: public updater names do not leak a matching account name`, () => {
    assert.deepEqual(scanBundlePrivacy(bytes(`${endpoint}\0${download}\0${PUBLIC_REPOSITORY}\0`), collision), []);
  });
  test(`${encoding}: a private marker outside the approved public identifier still fails`, () => {
    assert.match(scanBundlePrivacy(bytes(`${endpoint}\0build-account:${collisionName.toUpperCase()}\0`), collision).join(), /user name/);
    assert.match(scanBundlePrivacy(bytes(collisionName), collision).join(), /user name/);
  });
  test(`${encoding}: full home paths are never exempt and slash variants remain checked`, () => {
    for (const home of [markers.home, markers.home.replaceAll("\\", "/")]) assert.match(scanBundlePrivacy(bytes(home), markers).join(), /home path/);
    assert.match(scanBundlePrivacy(bytes(collision.home), collision).join(), /home path/);
  });
  test(`${encoding}: another repository, owner context, local path or longer name is not exempt`, () => {
    for (const text of [
      `https://github.com/${PUBLIC_REPOSITORY}-dev/releases/latest/download/latest.json`,
      `https://github.com/${collisionName}-other/public`,
      `https://unrelated.example/${PUBLIC_REPOSITORY}`,
      `C:/synthetic-work/${PUBLIC_REPOSITORY}/build`,
      `prefix${PUBLIC_REPOSITORY}`,
      `${PUBLIC_REPOSITORY}-extra`,
    ]) assert.match(scanBundlePrivacy(bytes(text), collision).join(), /user name/, text);
  });
  test(`${encoding}: a private home containing the exact approved identifier is not exempt`, () => {
    const home = `/synthetic-homes/${PUBLIC_REPOSITORY}`;
    assert.match(scanBundlePrivacy(bytes(home), { home, username: collisionName }).join(), /home path/);
  });
  test(`${encoding}: existing Mac home-only policy still catches the home path`, () => {
    assert.deepEqual(scanBundlePrivacy(bytes("generic runner framework symbol"), { home: "/Users/fixture", username: "runner", checkUsername: false }), []);
    assert.match(scanBundlePrivacy(bytes("/Users/fixture/source"), { home: "/Users/fixture", username: "runner", checkUsername: false }).join(), /home path/);
  });
}

test("checks raw UTF-16 patterns even at an odd byte offset", () => {
  assert.match(scanBundlePrivacy(Buffer.concat([Buffer.from([255]), Buffer.from(markers.home, "utf16le")]), markers).join(), /home path/);
});

test("missing marker inputs fail instead of silently disabling scanning", () => {
  assert.throws(() => scanBundlePrivacy(Buffer.from("fixture"), { home: "", username: "fixture" }), /markers are unavailable/);
});
