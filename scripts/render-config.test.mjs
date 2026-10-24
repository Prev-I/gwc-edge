// Tests for render-config.mjs: `node --test scripts/*.test.mjs` (run in CI).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { canonicalHost, mayBePublic, render, stripJsonc } from "./render-config.mjs";

const ID = "0f1e2d3c-4b5a-6978-8091-a2b3c4d5e6f7";

test("comments go, strings that look like comments stay", () => {
  const text = '{\n  // a\n  "u": "http://x.example/", /* b */ "n": 1,\n}';
  assert.deepEqual(JSON.parse(stripJsonc(text)), { u: "http://x.example/", n: 1 });
  assert.deepEqual(JSON.parse(stripJsonc('{"q": "a\\"//b"}')), { q: 'a"//b' });
  assert.deepEqual(JSON.parse(stripJsonc('{"s": "a, ]", "l": [1, 2,],}')), { s: "a, ]", l: [1, 2] });
});

test("the shipped template renders with id, route and canonical URL", () => {
  const template = readFileSync(new URL("../wrangler.jsonc", import.meta.url), "utf8");
  const out = JSON.parse(
    render(template, { canonicalUrl: "http://gwc.example.net/", databaseId: ID }),
  );
  assert.equal(out.vars.CANONICAL_URL, "http://gwc.example.net/");
  assert.equal(out.d1_databases[0].database_id, ID);
  assert.deepEqual(out.routes, [{ pattern: "gwc.example.net", custom_domain: true }]);
  assert.equal(out.workers_dev, false);
  assert.equal(out.preview_urls, false);
  assert.equal(out.vars.TEST_DIAL, undefined);
  assert.deepEqual(out.triggers.crons, ["*/10 * * * *"]);
});

test("only a plain-HTTP canonical URL on port 80 is accepted", () => {
  assert.equal(canonicalHost("http://gwc.example.org/"), "gwc.example.org");
  assert.equal(canonicalHost("http://gwc.example.org/gwc/"), "gwc.example.org");
  assert.equal(canonicalHost("http://a-b.example.org/gwc.php"), "a-b.example.org");
  for (const bad of [
    "https://gwc.example.org/",
    "http://gwc.example.org:8080/",
    "http://gwc.example.org",
    "http://GWC.example.org/",
    "http://8.8.4.4/",
    "http://localhost/",
    "http://gwc.example.org/?x",
    "http://gwc..example.org/",
    "http://-gwc.example.org/",
    "http://gwc.org-/",
    `http://${"a".repeat(64)}.example.org/`,
    "http://a.b/",
    `http://gwc.example.org/${"a".repeat(240)}`,
    "http://gwc.example.org/a|b",
    "http://gwc.example.org/a@b",
    "http://gwc.example.org/a\\b",
    "http://gwc.example.org/\u00e8",
    "http://gwc.example.org/a\x7f",
    "http://gwc.example.org/gwc./",
    "http://gwc.example.org/gwc.php.",
    "http://gwc.example.org//",
    "http://gwc.local/",
    "http://cache.internal/gwc/",
    "http://router.home.arpa/",
  ]) {
    assert.throws(() => canonicalHost(bad), /CANONICAL_URL/, bad);
  }
  assert.throws(
    () => render("{}", { canonicalUrl: "http://gwc.example.org/", databaseId: "nope" }),
    /database id/,
  );
});

test("non-public names are refused as the Worker refuses them (shared vectors)", () => {
  const vectors = JSON.parse(
    readFileSync(new URL("../crates/gwc-core/testdata/hostnames.json", import.meta.url), "utf8"),
  );
  for (const host of vectors.non_public) {
    assert.equal(mayBePublic(host), false, host);
    assert.throws(() => canonicalHost(`http://${host}/`), /CANONICAL_URL/, host);
  }
  for (const host of vectors.public) {
    assert.equal(mayBePublic(host), true, host);
    assert.equal(canonicalHost(`http://${host}/`), host);
  }
});
