#!/usr/bin/env node
/**
 * CLI wrapper for durable-acp-rs.
 *
 * Usage:
 *   npx @durable-acp/server --port 4437 claude-acp
 *   npx @durable-acp/server --port 4437 npx @agentclientprotocol/claude-agent-acp
 *
 * Resolves the platform binary and execs it with all arguments forwarded.
 */

import { execFileSync } from "child_process";
import { findBinary } from "./binary.ts";

const binary = findBinary();
const args = process.argv.slice(2);

try {
  execFileSync(binary, args, { stdio: "inherit" });
} catch (e: any) {
  process.exit(e.status ?? 1);
}
