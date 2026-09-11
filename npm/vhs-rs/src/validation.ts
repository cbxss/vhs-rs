import { VhsError } from "./errors.js";
import type { RunReport, CheckResult, Screen } from "./types.js";

export function object(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
export function parse(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    throw new VhsError("vhs-rs returned invalid JSON", "protocol_error");
  }
}
export function report(value: unknown): RunReport {
  const statuses: Record<string, number> = {
    success: 0,
    assert_failed: 1,
    parse_error: 2,
    wait_timeout: 3,
    runtime_error: 4,
  };
  if (
    !object(value) ||
    value.version !== 1 ||
    typeof value.tape !== "string" ||
    typeof value.status !== "string" ||
    statuses[value.status] === undefined ||
    statuses[value.status] !== value.exit_code ||
    !Array.isArray(value.commands) ||
    !Array.isArray(value.artifacts)
  )
    throw new VhsError("Invalid or unsupported run report", "protocol_error");
  return value as unknown as RunReport;
}
export function checkResult(value: unknown): CheckResult {
  if (
    !object(value) ||
    typeof value.ok !== "boolean" ||
    typeof value.commands !== "number" ||
    !Array.isArray(value.errors)
  )
    throw new VhsError("Invalid check result", "protocol_error");
  return value as unknown as CheckResult;
}
export function screen(value: unknown): Screen {
  if (
    !object(value) ||
    typeof value.screen_text !== "string" ||
    !object(value.cursor) ||
    typeof value.cursor.col !== "number" ||
    typeof value.cursor.row !== "number" ||
    typeof value.cursor.visible !== "boolean"
  )
    throw new VhsError("Invalid screen response", "protocol_error");
  return value as unknown as Screen;
}
