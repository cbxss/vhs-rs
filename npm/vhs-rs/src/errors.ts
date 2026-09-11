import type { RunReport } from "./types.js";

/** A transport, lifecycle, or session-command failure. Batch failures are reports. */
export class VhsError extends Error {
  constructor(
    message: string,
    readonly reason: string,
    readonly detail?: unknown,
    readonly report?: RunReport,
  ) {
    super(message);
    this.name = "VhsError";
  }
}
