import { appendFile } from "node:fs/promises";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";

const exec = promisify(execFile);
const verificationJobs = [
  "verify / verify (ubuntu-latest, linux-x64, x86_64-unknown-linux-musl)",
  "verify / verify (macos-14, darwin-arm64, aarch64-apple-darwin)",
];

export function validateRecovery(run, jobs, workflowId, tagSha, repository) {
  if (
    run.repository?.full_name !== repository ||
    run.workflow_id !== workflowId ||
    run.path !== ".github/workflows/release.yml" ||
    run.event !== "push" ||
    run.status !== "completed" ||
    !["failure", "success"].includes(run.conclusion) ||
    !/^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(run.head_branch) ||
    !/^[0-9a-f]{40}$/.test(run.head_sha) ||
    run.head_sha !== tagSha
  ) {
    throw new Error(
      "Source must be a completed Release run for the unchanged version tag",
    );
  }
  for (const name of verificationJobs) {
    // Failed-job retries can leave verification in an earlier run attempt.
    const latest = jobs
      .filter((job) => job.name === name)
      .sort((a, b) => b.id - a.id)[0];
    if (latest?.status !== "completed" || latest.conclusion !== "success") {
      throw new Error(`Source release verification did not pass: ${name}`);
    }
  }
  return { sha: run.head_sha, tag: run.head_branch, "run-id": String(run.id) };
}

async function main() {
  const {
    SOURCE_RUN_ID: sourceId,
    GITHUB_EVENT_NAME: event,
    GITHUB_REPOSITORY: repository,
    GITHUB_OUTPUT: output,
  } = process.env;
  let source;
  if (event === "push") {
    source = {
      sha: process.env.GITHUB_SHA,
      tag: process.env.GITHUB_REF_NAME,
      "run-id": process.env.GITHUB_RUN_ID,
    };
  } else if (
    event === "workflow_dispatch" &&
    /^[1-9]\d*$/.test(sourceId ?? "")
  ) {
    const api = async (path, args = []) =>
      JSON.parse(
        (await exec("gh", ["api", `repos/${repository}/${path}`, ...args]))
          .stdout,
      );
    const run = await api(`actions/runs/${sourceId}`);
    const workflow = await api("actions/workflows/release.yml");
    const commit = await api(`commits/${encodeURIComponent(run.head_branch)}`);
    const pages = await api(
      `actions/runs/${sourceId}/jobs?filter=all&per_page=100`,
      ["--paginate", "--slurp"],
    );
    source = validateRecovery(
      run,
      pages.flatMap((page) => page.jobs),
      workflow.id,
      commit.sha,
      repository,
    );
  } else {
    throw new Error("Expected a tag push or a numeric source Release run ID");
  }
  await appendFile(
    output,
    Object.entries(source)
      .map(([key, value]) => `${key}=${value}\n`)
      .join(""),
  );
  console.log(
    `Release ${source.tag} at ${source.sha}; artifact run ${source["run-id"]}`,
  );
}

if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(process.argv[1]).href
) {
  await main();
}
