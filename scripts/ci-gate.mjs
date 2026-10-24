#!/usr/bin/env node
// The deploy workflow's gate (ADR-0018): deploy only the current tip of main,
// and only once CI has passed on that exact commit, for automatic and manual
// runs alike.
//
//   node scripts/ci-gate.mjs        (in the deploy workflow)
//
// Env: SHA                the commit to deploy
//      GITHUB_REF         the workflow's ref (refs/heads/main for a deploy)
//      GITHUB_REPOSITORY  owner/repo
//      GH_TOKEN           read access to contents and actions
//      GITHUB_OUTPUT      where `deploy=true|false` is written
//
// Exit 0 with deploy=true: deploy. Exit 0 with deploy=false: a newer commit
// is main's tip and its own run deploys it. Exit 1: refused, including when
// the evidence cannot be had (fail closed).
import { execFileSync } from "node:child_process";
import { appendFileSync, realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";

/** The CI workflow whose success is required. */
export const CI_WORKFLOW = "ci.yml";

/**
 * Decide from the facts gathered. `runs` are CI workflow runs for `sha`, as
 * the GitHub API lists them. Returns { deploy, fail, reason }.
 */
export function decide({ sha, ref, tip, runs }) {
  const refuse = (reason) => ({ deploy: false, fail: true, reason });
  if (ref !== "refs/heads/main") return refuse(`only main deploys, not ${ref}`);
  if (!/^[0-9a-f]{40}$/.test(sha ?? "")) return refuse(`not a commit SHA: ${sha}`);
  if (!/^[0-9a-f]{40}$/.test(tip ?? "")) return refuse(`main's tip is unknown: ${tip}`);
  if (tip !== sha) {
    return {
      deploy: false,
      fail: false,
      reason: `${sha} is no longer the tip of main (${tip}); that commit's own run deploys it`,
    };
  }
  // CI on the push to main for this very commit; a pull request run for the
  // same SHA tested another ref.
  const mine = (runs ?? []).filter(
    (r) => r.head_sha === sha && r.event === "push" && r.head_branch === "main",
  );
  if (mine.length === 0) return refuse(`no CI run on main for ${sha}`);
  const latest = mine.reduce((a, b) =>
    b.run_number > a.run_number || (b.run_number === a.run_number && b.run_attempt > a.run_attempt)
      ? b
      : a,
  );
  if (latest.status !== "completed") return refuse(`CI on ${sha} is still ${latest.status}`);
  if (latest.conclusion !== "success") {
    return refuse(`CI on ${sha} concluded ${latest.conclusion}`);
  }
  return { deploy: true, fail: false, reason: `CI passed on ${sha} (run ${latest.run_number})` };
}

/** Gather the facts through `api(path)` (GitHub REST, parsed JSON), then decide. */
export async function gate({ sha, ref, repo, api }) {
  try {
    if (ref !== "refs/heads/main") return decide({ sha, ref });
    const tip = (await api(`repos/${repo}/commits/main`)).sha;
    const listed = await api(
      `repos/${repo}/actions/workflows/${CI_WORKFLOW}/runs?head_sha=${sha}&event=push&branch=main&per_page=100`,
    );
    return decide({ sha, ref, tip, runs: listed.workflow_runs });
  } catch (e) {
    return { deploy: false, fail: true, reason: `GitHub API unavailable: ${e.message}` };
  }
}

/** `gh api`, authenticated by GH_TOKEN. */
function ghApi(path) {
  return JSON.parse(execFileSync("gh", ["api", path], { encoding: "utf8" }));
}

async function main() {
  const result = await gate({
    sha: process.env.SHA,
    ref: process.env.GITHUB_REF,
    repo: process.env.GITHUB_REPOSITORY,
    api: ghApi,
  });
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, `deploy=${result.deploy}\n`);
  }
  if (result.fail) {
    console.log(`::error::Not deployed: ${result.reason}`);
    process.exit(1);
  }
  console.log(`${result.deploy ? "" : "::notice::Not deployed: "}${result.reason}`);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === realpathSync(process.argv[1])) {
  main();
}
