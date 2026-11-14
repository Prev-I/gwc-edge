// Tests for ci-gate.mjs, with the GitHub API mocked: `node --test scripts/*.test.mjs`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { gate } from "./ci-gate.mjs";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const SHA = "1".repeat(40);
const OLD = "2".repeat(40);
const MAIN = "refs/heads/main";

const run = (over) => ({
  head_sha: SHA,
  head_branch: "main",
  event: "push",
  status: "completed",
  conclusion: "success",
  run_number: 7,
  run_attempt: 1,
  ...over,
});

/** The one runs query the gate may make: this commit, on main, every event. */
const runsPath = (sha) =>
  `repos/o/r/actions/workflows/ci.yml/runs?head_sha=${sha}&branch=main&per_page=100`;

/** A fake `gh api`: main's tip and the CI runs of the commit being checked. */
const api = ({ tip = SHA, runs = [run()], fail = false, sha }) =>
  async (path) => {
    if (fail) throw new Error("HTTP 502");
    if (path === "repos/o/r/commits/main") return { sha: tip };
    // Exactly this query: no event filter (manual runs would be lost), and
    // still scoped to the commit and to main.
    if (path === runsPath(sha)) return { workflow_runs: runs };
    throw new Error(`unexpected path ${path}`);
  };

const check = (opts, ref = MAIN, sha = SHA) =>
  gate({ sha, ref, repo: "o/r", api: api({ ...opts, sha }) });

test("green CI on the exact tip of main deploys", async () => {
  const r = await check({});
  assert.equal(r.deploy, true);
  assert.equal(r.fail, false);
});

test("failed, cancelled or still-running CI refuses", async () => {
  for (const over of [
    { conclusion: "failure" },
    { conclusion: "cancelled" },
    { status: "in_progress", conclusion: null },
    { status: "queued", conclusion: null },
  ]) {
    const r = await check({ runs: [run(over)] });
    assert.deepEqual([r.deploy, r.fail], [false, true], JSON.stringify(over));
  }
});

test("no CI run for the commit refuses", async () => {
  const r = await check({ runs: [] });
  assert.deepEqual([r.deploy, r.fail], [false, true]);
  assert.match(r.reason, /no CI run/);
});

test("green CI for another commit, ref or event does not count", async () => {
  for (const other of [
    run({ head_sha: OLD }),
    run({ head_branch: "feature" }),
    run({ event: "pull_request" }),
  ]) {
    const r = await check({ runs: [other] });
    assert.deepEqual([r.deploy, r.fail], [false, true], JSON.stringify(other));
  }
});

test("green manual CI on the exact tip of main deploys", async () => {
  const r = await check({ runs: [run({ event: "workflow_dispatch" })] });
  assert.equal(r.deploy, true);
  assert.equal(r.fail, false);
});

test("manual CI on another branch does not count", async () => {
  const r = await check({ runs: [run({ event: "workflow_dispatch", head_branch: "feature" })] });
  assert.deepEqual([r.deploy, r.fail], [false, true]);
  assert.match(r.reason, /no CI run/);
});

test("CI from other events does not count", async () => {
  for (const event of ["schedule", "repository_dispatch", "merge_group", "pull_request"]) {
    const r = await check({ runs: [run({ event })] });
    assert.deepEqual([r.deploy, r.fail], [false, true], event);
  }
});

test("the latest run decides across push and manual runs", async () => {
  const r = await check({
    runs: [run({ run_number: 7 }), run({ run_number: 8, event: "workflow_dispatch", conclusion: "failure" })],
  });
  assert.equal(r.fail, true);
  const again = await check({
    runs: [run({ run_number: 7, conclusion: "failure" }), run({ run_number: 8, event: "workflow_dispatch" })],
  });
  assert.equal(again.deploy, true);
});

test("the latest run decides: a later failed re-run beats an earlier success", async () => {
  const r = await check({
    runs: [run({ run_number: 7, run_attempt: 1 }), run({ run_number: 7, run_attempt: 2, conclusion: "failure" })],
  });
  assert.equal(r.fail, true);
  const again = await check({
    runs: [run({ run_number: 7, conclusion: "failure" }), run({ run_number: 8 })],
  });
  assert.equal(again.deploy, true);
});

test("a commit that is no longer main's tip is skipped, not deployed", async () => {
  const r = await check({ tip: OLD });
  assert.deepEqual([r.deploy, r.fail], [false, false]);
});

test("any ref but main refuses, before calling the API", async () => {
  for (const ref of ["refs/heads/feature", "refs/tags/v0.1.0", "refs/pull/1/merge", undefined]) {
    let calls = 0;
    const r = await gate({
      sha: SHA,
      ref,
      repo: "o/r",
      // The gate catches a throwing API as "unavailable", so count calls
      // instead: a refusal after calling it would also read [false, true].
      api: async () => {
        calls += 1;
        return {};
      },
    });
    assert.deepEqual([r.deploy, r.fail], [false, true], String(ref));
    assert.equal(calls, 0, String(ref));
    assert.match(r.reason, /only main deploys/, String(ref));
  }
});

test("an API failure refuses (fail closed)", async () => {
  const r = await check({ fail: true });
  assert.deepEqual([r.deploy, r.fail], [false, true]);
  assert.match(r.reason, /GitHub API unavailable/);
});

test("a malformed SHA refuses", async () => {
  const r = await check({}, MAIN, "main");
  assert.equal(r.fail, true);
});

test("an unreadable tip refuses rather than skipping", async () => {
  for (const tip of [null, "", "main"]) {
    const r = await check({ tip });
    assert.deepEqual([r.deploy, r.fail], [false, true], String(tip));
  }
});

// The CLI as the workflow runs it, with a fake `gh` first on PATH.

function cli({ tip, conclusion, ghFails = false }) {
  const dir = mkdtempSync(join(tmpdir(), "ci-gate-"));
  const runs = JSON.stringify({ workflow_runs: [run({ conclusion })] });
  writeFileSync(
    join(dir, "gh"),
    ghFails
      ? "#!/bin/sh\necho 'HTTP 403' >&2\nexit 1\n"
      : `#!/bin/sh\ncase "$2" in\n  */commits/main) echo '{"sha":"${tip}"}' ;;\n  *) echo '${runs}' ;;\nesac\n`,
  );
  chmodSync(join(dir, "gh"), 0o755);
  const output = join(dir, "output");
  writeFileSync(output, "");
  let status = 0;
  try {
    execFileSync(process.execPath, [new URL("./ci-gate.mjs", import.meta.url).pathname], {
      env: {
        PATH: `${dir}:${process.env.PATH}`,
        SHA,
        GITHUB_REF: MAIN,
        GITHUB_REPOSITORY: "o/r",
        GITHUB_OUTPUT: output,
      },
      stdio: "pipe",
    });
  } catch (e) {
    status = e.status;
  }
  return { status, output: readFileSync(output, "utf8") };
}

test("the CLI deploys, skips or fails with the right exit code and output", () => {
  assert.deepEqual(cli({ tip: SHA, conclusion: "success" }), { status: 0, output: "deploy=true\n" });
  assert.deepEqual(cli({ tip: OLD, conclusion: "success" }), { status: 0, output: "deploy=false\n" });
  assert.deepEqual(cli({ tip: SHA, conclusion: "failure" }), { status: 1, output: "deploy=false\n" });
  assert.deepEqual(cli({ tip: SHA, conclusion: "success", ghFails: true }), {
    status: 1,
    output: "deploy=false\n",
  });
});
