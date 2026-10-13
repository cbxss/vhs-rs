import test from "node:test";
import assert from "node:assert/strict";
import { validateRecovery } from "./release-source.mjs";

const sha = "c51dde9bff29d1ab749868a458b24997029d7756";
const run = {
  id: 34643339627,
  repository: { full_name: "cbxss/vhs-rs" },
  workflow_id: 308340804,
  path: ".github/workflows/release.yml",
  event: "push",
  status: "completed",
  conclusion: "failure",
  head_branch: "v0.3.1",
  head_sha: sha,
};
const jobs = [
  {
    id: 1,
    name: "verify / verify (ubuntu-latest, linux-x64, x86_64-unknown-linux-musl)",
    status: "completed",
    conclusion: "success",
  },
  {
    id: 2,
    name: "verify / verify (macos-14, darwin-arm64, aarch64-apple-darwin)",
    status: "completed",
    conclusion: "success",
  },
  { id: 3, name: "publish", status: "completed", conclusion: "failure" },
];
const validate = (source = run, checks = jobs, tagSha = sha) =>
  validateRecovery(source, checks, run.workflow_id, tagSha, "cbxss/vhs-rs");

test("recover verified original artifacts after publication failure", () => {
  assert.deepEqual(validate(), { sha, tag: "v0.3.1", "run-id": "34643339627" });
  assert.deepEqual(validate({ ...run, conclusion: "success" }), validate());
});

test("reject untrusted source identity, non-tag runs, and moved tags", () => {
  for (const changed of [
    { repository: { full_name: "other/vhs-rs" } },
    { workflow_id: 1 },
    { path: ".github/workflows/ci.yml" },
    { event: "pull_request" },
    { event: "workflow_dispatch" },
    { status: "in_progress" },
    { conclusion: "cancelled" },
    { head_branch: "main" },
    { head_branch: "v0.3.1\nsha=attacker" },
    { head_sha: "invalid" },
  ]) {
    assert.throws(() => validate({ ...run, ...changed }), /Source must be/);
  }
  assert.throws(
    () => validate(run, jobs, "a".repeat(40)),
    /unchanged version tag/,
  );
});

test("require both platforms to have passed verification", () => {
  assert.throws(
    () => validate(run, jobs.slice(1)),
    /verification did not pass/,
  );
  for (const conclusion of ["failure", "skipped", "cancelled"]) {
    assert.throws(
      () => validate(run, [{ ...jobs[0], conclusion }, ...jobs.slice(1)]),
      /verification did not pass/,
    );
  }
  assert.throws(
    () =>
      validate(run, [{ ...jobs[0], status: "in_progress" }, ...jobs.slice(1)]),
    /verification did not pass/,
  );
});

test("use the latest verification attempt, retaining earlier successful jobs on publish-only retries", () => {
  assert.deepEqual(validate(run, [...jobs, { ...jobs[2], id: 4 }]), validate());
  assert.throws(
    () =>
      validate(run, [...jobs, { ...jobs[0], id: 4, conclusion: "failure" }]),
    /verification did not pass/,
  );
  assert.deepEqual(
    validate(run, [{ ...jobs[0], id: 0, conclusion: "failure" }, ...jobs]),
    validate(),
  );
});
