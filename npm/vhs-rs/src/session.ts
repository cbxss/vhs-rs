import { StringDecoder } from "node:string_decoder";
import { version } from "./binary.js";
import { VhsError } from "./errors.js";
import { ManagedProcess, milliseconds } from "./process.js";
import { object, parse, report, screen } from "./validation.js";
import type {
  Key,
  MatchOptions,
  RunReport,
  Screen,
  SessionOptions,
} from "./types.js";

type Result = Record<string, unknown>;
type Pending = {
  resolve: (result: Result) => void;
  reject: (error: VhsError) => void;
};

export class Session {
  private readonly proc: ManagedProcess;
  private readonly pending = new Map<number, Pending>();
  private nextId = 1;
  private closing: Promise<RunReport> | undefined;
  private ended = false;
  private readonly idleWaiters = new Set<() => void>();
  private readonly closeTimeoutMs: number;
  private readySeen = false;
  private readonly ready: Promise<void>;
  private resolveReady!: () => void;
  private rejectReady!: (error: VhsError) => void;

  private constructor(options: SessionOptions) {
    const args = ["repl", "--json-input", "--quiet"];
    if (options.strict) args.push("--strict");
    if (options.record !== undefined) args.push("--record", options.record);
    if (options.timeoutMs !== undefined)
      args.push("--timeout", milliseconds(options.timeoutMs));
    this.proc = new ManagedProcess(args, options);
    this.closeTimeoutMs = options.shutdownTimeoutMs ?? 5000;
    this.ready = new Promise((resolve, reject) => {
      this.resolveReady = resolve;
      this.rejectReady = reject;
    });
    const startup = setTimeout(
      () =>
        this.proc.fail(
          new VhsError(
            "No startup handshake within 10 seconds",
            "startup_timeout",
          ),
        ),
      10000,
    );
    startup.unref();
    const decoder = new StringDecoder("utf8");
    let buffered = "";
    this.proc.child.stdout.on("data", (chunk: Buffer) => {
      if (this.proc.failure) return;
      buffered += decoder.write(chunk);
      if (Buffer.byteLength(buffered) > 64 * 1024 * 1024) {
        this.proc.fail(
          new VhsError("Protocol response exceeds 64 MiB", "protocol_error"),
        );
        return;
      }
      let end: number;
      while ((end = buffered.indexOf("\n")) >= 0) {
        const line = buffered.slice(0, end);
        buffered = buffered.slice(end + 1);
        try {
          this.receive(parse(line));
        } catch (e) {
          this.proc.fail(
            e instanceof VhsError
              ? e
              : new VhsError(String(e), "protocol_error"),
          );
          return;
        }
        if (this.readySeen) clearTimeout(startup);
      }
    });
    void this.proc.done.then(({ code, signal }) => {
      clearTimeout(startup);
      this.ended = true;
      const error =
        this.proc.failure ??
        new VhsError(
          `Session exited (${code ?? signal}): ${this.proc.stderr}`,
          "process_exited",
        );
      this.rejectReady(error);
      for (const pending of this.pending.values()) pending.reject(error);
      this.pending.clear();
      this.notifyIdle();
    });
  }

  /** Starts a session and validates all initial configuration before actions. */
  static async create(options: SessionOptions = {}): Promise<Session> {
    const session = new Session(options);
    try {
      await session.ready;
      const settings = {
        shell: options.shell,
        typing_speed_ms: options.typingSpeedMs,
        wait_timeout_ms: options.waitTimeoutMs,
        width: options.width,
        height: options.height,
        font_size: options.fontSize,
        theme: options.theme,
      };
      await session.request({ op: "configure", settings });
      return session;
    } catch (error) {
      session.proc.fail(
        error instanceof VhsError
          ? error
          : new VhsError(String(error), "invalid_argument"),
      );
      await session.proc.done;
      throw error;
    }
  }

  private receive(value: unknown): void {
    if (!object(value))
      throw new VhsError("Invalid protocol envelope", "protocol_error");
    if (value.kind === "ready") {
      if (
        this.readySeen ||
        value.version !== 1 ||
        value.binary_version !== version
      )
        throw new VhsError(
          "Unsupported protocol or binary version",
          "version_mismatch",
        );
      this.readySeen = true;
      this.resolveReady();
      return;
    }
    if (!this.readySeen)
      throw new VhsError("Response before handshake", "protocol_error");
    if (value.kind === "report") {
      const final = report(value);
      const error = new VhsError(
        final.failure?.message ?? "Session ended",
        final.failure?.reason ?? "process_exited",
        undefined,
        final,
      );
      for (const pending of this.pending.values()) pending.reject(error);
      this.pending.clear();
      this.notifyIdle();
      this.ended = true;
      return;
    }
    if (
      value.kind !== "result" ||
      typeof value.id !== "number" ||
      !["ok", "failed"].includes(String(value.status))
    )
      throw new VhsError("Invalid response", "protocol_error");
    const pending = this.pending.get(value.id);
    if (!pending)
      throw new VhsError(
        `Unexpected response ID ${value.id}`,
        "protocol_error",
      );
    if (value.status === "failed") {
      if (
        !object(value.failure) ||
        typeof value.failure.reason !== "string" ||
        typeof value.failure.message !== "string"
      )
        throw new VhsError("Invalid failure response", "protocol_error");
      pending.reject(
        new VhsError(
          value.failure.message,
          value.failure.reason,
          value.detail,
          value.report === undefined ? undefined : report(value.report),
        ),
      );
    } else pending.resolve(value);
    this.pending.delete(value.id);
    this.notifyIdle();
  }

  private notifyIdle(): void {
    if (this.pending.size === 0) {
      for (const resolve of this.idleWaiters) resolve();
      this.idleWaiters.clear();
    }
  }

  private request(
    command: Record<string, unknown>,
    closing = false,
  ): Promise<Result> {
    if (this.ended || this.proc.failure || (this.closing && !closing))
      return Promise.reject(
        this.proc.failure ??
          new VhsError("Session is closed or closing", "session_closed"),
      );
    if (this.pending.size >= 32)
      return Promise.reject(
        new VhsError("At most 32 requests may be pending", "queue_full"),
      );
    const id = this.nextId++;
    let data: string;
    try {
      data = JSON.stringify({ id, command }) + "\n";
    } catch {
      return Promise.reject(
        new VhsError("Request is not JSON serializable", "invalid_argument"),
      );
    }
    if (Buffer.byteLength(data) > 1024 * 1024)
      return Promise.reject(
        new VhsError("Request exceeds 1 MiB", "invalid_argument"),
      );
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.proc.child.stdin.write(data);
    });
  }

  async type(text: string, options: { speedMs?: number } = {}): Promise<void> {
    if (options.speedMs !== undefined) milliseconds(options.speedMs, "speedMs");
    await this.request({ op: "type", text, speed_ms: options.speedMs });
  }
  async press(key: Key, options: { count?: number } = {}): Promise<void> {
    await this.request({ op: "press", key, count: options.count });
  }
  /** Patterns use Rust regex syntax. Omit the pattern to wait for the prompt. */
  async waitFor(pattern?: string, options: MatchOptions = {}): Promise<void> {
    if (options.timeoutMs !== undefined) milliseconds(options.timeoutMs);
    await this.request({
      op: "wait",
      pattern,
      scope: options.scope,
      timeout_ms: options.timeoutMs,
    });
  }
  async assert(pattern: string, options: MatchOptions = {}): Promise<void> {
    if (options.timeoutMs !== undefined) milliseconds(options.timeoutMs);
    await this.request({
      op: "assert",
      pattern,
      scope: options.scope,
      timeout_ms: options.timeoutMs,
    });
  }
  async screen(): Promise<Screen> {
    return screen((await this.request({ op: "screen" })).detail);
  }
  async screenshot(path: string): Promise<void> {
    await this.request({ op: "screenshot", path });
  }
  async capture(path: string): Promise<void> {
    await this.request({ op: "capture", path });
  }
  /** Register a final output; declare text outputs before the first action. */
  async output(path: string): Promise<void> {
    await this.request({ op: "output", path });
  }

  /** Drain accepted commands, finalize artifacts, and wait for process exit. */
  close(): Promise<RunReport> {
    if (this.closing) return this.closing;
    this.closing = (async () => {
      // Close follows all accepted work, including recoverable failures.
      await new Promise<void>((resolve) => {
        this.idleWaiters.add(resolve);
        this.notifyIdle();
      });
      const timer = setTimeout(
        () =>
          this.proc.fail(
            new VhsError("Session finalization timed out", "shutdown_timeout"),
          ),
        this.closeTimeoutMs,
      );
      timer.unref();
      try {
        const result = await this.request({ op: "close" }, true);
        const final = report(result.report);
        this.proc.child.stdin.end();
        const exit = await this.proc.done;
        if (this.proc.failure) throw this.proc.failure;
        if (exit.code !== final.exit_code)
          throw new VhsError(
            "Close report and process exit disagree",
            "protocol_error",
          );
        return final;
      } catch (error) {
        this.proc.fail(
          error instanceof VhsError
            ? error
            : new VhsError(String(error), "protocol_error"),
        );
        await this.proc.done;
        throw error;
      } finally {
        clearTimeout(timer);
      }
    })();
    return this.closing;
  }
}
export function createSession(options: SessionOptions = {}): Promise<Session> {
  return Session.create(options);
}
