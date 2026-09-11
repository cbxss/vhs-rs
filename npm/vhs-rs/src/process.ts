import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import { resolveBinary } from "./binary.js";
import { VhsError } from "./errors.js";
import type { ProcessOptions } from "./types.js";

export function milliseconds(value: number, name = "timeoutMs"): string {
  if (!Number.isSafeInteger(value) || value < 0 || value > 0xffff_ffff)
    throw new VhsError(
      `${name} must be an integer between 0 and 4294967295`,
      "invalid_argument",
    );
  return `${value}ms`;
}

export class ManagedProcess {
  readonly child: ChildProcessWithoutNullStreams;
  readonly done: Promise<{
    code: number | null;
    signal: NodeJS.Signals | null;
  }>;
  failure: VhsError | undefined;
  stderr = "";
  private killTimer: NodeJS.Timeout | undefined;
  private budgetTimer: NodeJS.Timeout | undefined;
  private closed = false;
  private readonly grace: number;

  constructor(args: string[], options: ProcessOptions) {
    if (options.signal?.aborted)
      throw new VhsError("Operation aborted", "aborted");
    if (options.timeoutMs !== undefined) milliseconds(options.timeoutMs);
    this.grace = options.shutdownTimeoutMs ?? 5000;
    milliseconds(this.grace, "shutdownTimeoutMs");
    if (
      this.grace > 2_147_483_647 ||
      (options.timeoutMs ?? 0) + this.grace > 2_147_483_647
    ) {
      throw new VhsError(
        "Timeout plus shutdown grace must fit a Node timer (2147483647 ms)",
        "invalid_argument",
      );
    }
    this.child = spawn(resolveBinary(), args, {
      ...(options.cwd === undefined ? {} : { cwd: options.cwd }),
      env: { ...process.env, ...options.env },
      stdio: "pipe",
      shell: false,
    });
    this.child.stderr.setEncoding("utf8");
    this.child.stderr.on("data", (chunk: string) => {
      this.stderr = (this.stderr + chunk).slice(-65536);
    });
    this.child.stdin.on("error", (error: Error) => {
      this.fail(new VhsError(error.message, "stdin_error"));
    });
    this.child.on("error", (error: Error) => {
      this.fail(new VhsError(error.message, "spawn_error"));
    });
    const abort = () => this.fail(new VhsError("Operation aborted", "aborted"));
    options.signal?.addEventListener("abort", abort, { once: true });
    this.done = new Promise((resolve) => {
      this.child.once("close", (code, signal) => {
        this.closed = true;
        clearTimeout(this.killTimer);
        clearTimeout(this.budgetTimer);
        options.signal?.removeEventListener("abort", abort);
        resolve({ code, signal });
      });
    });
    // The Rust timeout gets first chance to produce a report; this is the
    // outer watchdog for startup, final rendering, or an unresponsive process.
    if (options.timeoutMs !== undefined) {
      this.budgetTimer = setTimeout(
        () =>
          this.fail(
            new VhsError(
              "Operation exceeded its timeout and shutdown grace",
              "run_timeout",
            ),
          ),
        options.timeoutMs + this.grace,
      );
      this.budgetTimer.unref();
    }
  }

  fail(error: VhsError): void {
    this.failure ??= error;
    if (this.closed || this.killTimer) return;
    this.child.kill("SIGTERM");
    this.killTimer = setTimeout(() => this.child.kill("SIGKILL"), this.grace);
    this.killTimer.unref();
  }
}

export async function execute(
  args: string[],
  options: ProcessOptions,
  input?: string,
): Promise<{ stdout: string; code: number; stderr: string }> {
  const proc = new ManagedProcess(args, options);
  const chunks: Buffer[] = [];
  let size = 0;
  proc.child.stdout.on("data", (chunk: Buffer) => {
    size += chunk.length;
    if (size > 64 * 1024 * 1024)
      proc.fail(new VhsError("Response exceeds 64 MiB", "protocol_error"));
    else chunks.push(chunk);
  });
  proc.child.stdin.end(input);
  const { code, signal } = await proc.done;
  if (proc.failure) throw proc.failure;
  if (code === null)
    throw new VhsError(
      `vhs-rs exited on ${signal}: ${proc.stderr}`,
      "process_exited",
    );
  return {
    stdout: Buffer.concat(chunks).toString("utf8"),
    code,
    stderr: proc.stderr,
  };
}
