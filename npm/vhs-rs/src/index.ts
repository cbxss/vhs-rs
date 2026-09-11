import { resolve } from "node:path";
import { execute, milliseconds } from "./process.js";
import { checkResult, parse, report } from "./validation.js";
import { VhsError } from "./errors.js";
import type {
  CheckResult,
  ProcessOptions,
  RenderOptions,
  RunFileOptions,
  RunOptions,
  RunReport,
} from "./types.js";

export * from "./types.js";
export { VhsError } from "./errors.js";
export { createSession, Session } from "./session.js";

function runArgs(options: RunFileOptions): string[] {
  const args = ["run", "--json", "--quiet"];
  if (options.timeoutMs !== undefined)
    args.push("--timeout", milliseconds(options.timeoutMs));
  if (options.record !== undefined) args.push("--record", options.record);
  return args;
}
async function runResult(
  args: string[],
  options: ProcessOptions,
  input?: string,
): Promise<RunReport> {
  const result = await execute(args, options, input);
  const value = report(parse(result.stdout));
  if (value.exit_code !== result.code)
    throw new VhsError(
      "Report and process exit codes disagree",
      "protocol_error",
    );
  return value;
}
export async function run(options: RunOptions): Promise<RunReport> {
  if (typeof options.tape !== "string")
    throw new VhsError("tape must be a string", "invalid_argument");
  return runResult([...runArgs(options), "--", "-"], options, options.tape);
}
export async function runFile(
  path: string,
  options: RunFileOptions = {},
): Promise<RunReport> {
  if (typeof path !== "string" || !path.length)
    throw new VhsError("path must be a nonempty string", "invalid_argument");
  return runResult([...runArgs(options), "--", path], options);
}
export async function check(
  options: { tape: string } & ProcessOptions,
): Promise<CheckResult> {
  if (typeof options.tape !== "string")
    throw new VhsError("tape must be a string", "invalid_argument");
  const result = await execute(
    ["check", "--json", "--", "-"],
    options,
    options.tape,
  );
  const value = checkResult(parse(result.stdout));
  if (result.code !== (value.ok ? 0 : 2))
    throw new VhsError(`Check failed: ${result.stderr}`, "process_exited");
  return value;
}
export async function render(
  recording: string,
  options: RenderOptions,
): Promise<{ paths: string[] }> {
  if (!options.outputs.length)
    throw new VhsError("render needs at least one output", "invalid_argument");
  const args = ["render", "--quiet"];
  for (const path of options.outputs) args.push("--output", path);
  if (options.theme !== undefined) args.push("--theme", options.theme);
  if (options.idleLimitMs !== undefined)
    args.push("--idle-limit", milliseconds(options.idleLimitMs, "idleLimitMs"));
  for (const [key, value] of [
    ["speed", options.speed],
    ["framerate", options.framerate],
    ["font-size", options.fontSize],
  ] as const) {
    if (value !== undefined) {
      if (!Number.isFinite(value) || value <= 0)
        throw new VhsError(`${key} must be positive`, "invalid_argument");
      args.push(`--${key}`, String(value));
    }
  }
  args.push("--", recording);
  const result = await execute(args, options);
  if (result.code !== 0)
    throw new VhsError(`Render failed: ${result.stderr}`, "render_failed");
  return {
    paths: options.outputs.map((path) =>
      resolve(options.cwd ?? process.cwd(), path),
    ),
  };
}
