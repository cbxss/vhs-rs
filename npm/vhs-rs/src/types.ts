export type RunStatus =
  | "success"
  | "assert_failed"
  | "wait_timeout"
  | "runtime_error"
  | "parse_error";
export interface Diagnostic {
  line: number;
  col: number;
  message: string;
}
export interface Failure {
  reason: string;
  message: string;
}
export interface Artifact {
  path: string;
  kind:
    | "gif"
    | "png"
    | "text"
    | "golden"
    | "cast"
    | "timeline"
    | "failure_text"
    | "failure_png";
  command_index?: number;
}
export interface CommandRecord {
  index: number;
  line: number;
  col: number;
  command: string;
  status: "ok" | "failed" | "skipped";
  elapsed_ms: number;
  detail?: Record<string, unknown>;
}
export interface RunReport {
  version: 1;
  tape: string;
  status: RunStatus;
  exit_code: number;
  duration_ms?: number;
  term?: { cols: number; rows: number; shell: string };
  commands: CommandRecord[];
  artifacts: Artifact[];
  failure?: Failure & { command_index?: number };
  errors?: Diagnostic[];
}
export interface CheckResult {
  ok: boolean;
  commands: number;
  errors: Diagnostic[];
}
export interface Screen {
  screen_text: string;
  cursor: { col: number; row: number; visible: boolean };
}
export interface ProcessOptions {
  cwd?: string;
  /** Merged with the Node process environment. */
  env?: Record<string, string>;
  /** Cancels the whole run/session. An interrupted session cannot be reused. */
  signal?: AbortSignal;
  /** Whole operation/session wall-clock budget, in milliseconds. */
  timeoutMs?: number;
  /** Grace period before force killing a process that fails to shut down. Default 5000. */
  shutdownTimeoutMs?: number;
}
export interface RunOptions extends ProcessOptions {
  tape: string;
  record?: string;
}
export interface RunFileOptions extends ProcessOptions {
  record?: string;
}
export interface SessionOptions extends ProcessOptions {
  record?: string;
  strict?: boolean;
  shell?: string;
  typingSpeedMs?: number;
  waitTimeoutMs?: number;
  width?: number;
  height?: number;
  fontSize?: number;
  theme?: string;
}
export interface MatchOptions {
  scope?: "line" | "screen";
  timeoutMs?: number;
}
export interface RenderOptions extends ProcessOptions {
  outputs: string[];
  theme?: string;
  idleLimitMs?: number;
  speed?: number;
  framerate?: number;
  fontSize?: number;
}
export type Key =
  | "Enter"
  | "Space"
  | "Backspace"
  | "Delete"
  | "Insert"
  | "Escape"
  | "Tab"
  | "Down"
  | "Left"
  | "Right"
  | "Up"
  | "PageUp"
  | "PageDown"
  | "Home"
  | "End"
  | "ScrollUp"
  | "ScrollDown"
  | `Ctrl+${string}`
  | `Alt+${string}`
  | `Shift+${string}`;
