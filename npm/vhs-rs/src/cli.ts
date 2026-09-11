#!/usr/bin/env node
import { spawn } from "node:child_process";
import { resolveBinary } from "./binary.js";

try {
  const child = spawn(resolveBinary(), process.argv.slice(2), {
    stdio: "inherit",
  });
  const forwardInt = () => child.kill("SIGINT");
  const forwardTerm = () => child.kill("SIGTERM");
  process.on("SIGINT", forwardInt);
  process.on("SIGTERM", forwardTerm);
  child.on("error", (error) => {
    console.error(error.message);
    process.exitCode = 4;
  });
  child.on("close", (code, signal) => {
    process.off("SIGINT", forwardInt);
    process.off("SIGTERM", forwardTerm);
    process.exitCode = code ?? (signal === "SIGINT" ? 130 : 143);
  });
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 4;
}
