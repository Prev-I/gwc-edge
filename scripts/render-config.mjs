#!/usr/bin/env node
// Render the deployment config from the committed wrangler.jsonc, which stays
// a template (no database id, no route, a placeholder CANONICAL_URL):
//
//   node scripts/render-config.mjs <in.jsonc> <out.json> \
//     --canonical-url http://gwc.example.org/ --database-id <uuid>
//
// It sets vars.CANONICAL_URL, the D1 binding's database_id and one Custom
// Domain route for the canonical host (ADR-0015). Comments are dropped; the
// output is plain JSON, which wrangler reads like JSONC.
import { readFileSync, realpathSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

/**
 * Remove // and /* *\/ comments, then trailing commas before } or ]; string
 * contents are left intact in both passes.
 */
export function stripJsonc(text) {
  return scan(scan(text, true), false);
}

function scan(text, comments) {
  let out = "";
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === '"') {
      const start = i;
      for (i++; i < text.length && text[i] !== '"'; i++) {
        if (text[i] === "\\") i++;
      }
      out += text.slice(start, i + 1);
    } else if (comments && c === "/" && text[i + 1] === "/") {
      while (i < text.length && text[i] !== "\n") i++;
      out += "\n";
    } else if (comments && c === "/" && text[i + 1] === "*") {
      i = text.indexOf("*/", i + 2);
      if (i < 0) throw new Error("unterminated comment");
      i++;
    } else if (!comments && c === ",") {
      let j = i + 1;
      while (j < text.length && /\s/.test(text[j])) j++;
      if (text[j] !== "}" && text[j] !== "]") out += c;
    } else {
      out += c;
    }
  }
  return out;
}

/**
 * The host of a canonical URL, or an error. The rule is narrower than the
 * Worker's own (`cache_url::normalize`) and accepts only URLs it leaves
 * unchanged, so a URL that renders also passes the Worker's configuration
 * check and is listed exactly as given: plain http://, port 80, a lowercase
 * DNS name, printable ASCII without | ? # @ \, 12 to 255 characters, and a
 * path that Shareaza's clean-up (strip trailing . and /, keep one /) keeps.
 */
export function canonicalHost(url) {
  const fail = (why) => {
    throw new Error(`CANONICAL_URL ${why}: ${url}`);
  };
  if (!/^[\x21-\x7e]+$/.test(url) || /[|?#@\\]/.test(url)) fail("has a forbidden character");
  if (url.length < 12 || url.length > 255) fail("must be 12 to 255 characters");
  const m = /^http:\/\/([^/:]+)(\/.*)$/.exec(url);
  if (!m) fail("must be http://<host>/<path>, port 80");
  const [, host, path] = m;
  const label = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/;
  const labels = host.split(".");
  if (labels.length < 2 || !labels.every((l) => label.test(l)) || !/^[a-z]/.test(labels.at(-1))) {
    fail("must be on a lowercase DNS name");
  }
  const kept = path.replace(/[./]+$/, "");
  if (path !== (path.endsWith("/") || kept === "" ? `${kept}/` : kept)) {
    fail("would be rewritten by Shareaza's clean-up (trailing . or //)");
  }
  return host;
}

export function render(template, { canonicalUrl, databaseId }) {
  const config = JSON.parse(stripJsonc(template));
  const host = canonicalHost(canonicalUrl);
  if (!/^[0-9a-f-]{36}$/.test(databaseId)) {
    throw new Error(`not a D1 database id: ${databaseId}`);
  }
  config.vars = { ...config.vars, CANONICAL_URL: canonicalUrl };
  const db = config.d1_databases?.find((d) => d.binding === "DB");
  if (!db) throw new Error("no D1 binding named DB");
  db.database_id = databaseId;
  config.routes = [{ pattern: host, custom_domain: true }];
  config.workers_dev = false;
  config.preview_urls = false;
  return JSON.stringify(config, null, 2) + "\n";
}

function main(argv) {
  const [input, output, ...rest] = argv;
  const opt = (name) => {
    const i = rest.indexOf(name);
    if (i < 0 || !rest[i + 1]) throw new Error(`missing ${name}`);
    return rest[i + 1];
  };
  const text = render(readFileSync(input, "utf8"), {
    canonicalUrl: opt("--canonical-url"),
    databaseId: opt("--database-id"),
  });
  writeFileSync(output, text);
}

// Run as a script, not when imported by the tests.
if (process.argv[1] && fileURLToPath(import.meta.url) === realpathSync(process.argv[1])) {
  try {
    main(process.argv.slice(2));
  } catch (e) {
    console.error(`render-config: ${e.message}`);
    process.exit(1);
  }
}
