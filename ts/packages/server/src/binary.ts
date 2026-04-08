/**
 * Platform binary resolution for durable-acp-rs.
 *
 * Resolves the Rust binary relative to the package root.
 * Works both in-repo (cargo build) and when published (platform packages).
 */

import path from "path";
import { existsSync } from "fs";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

/**
 * Find the durable-acp-rs binary.
 *
 * Resolution order:
 * 1. Release build: target/release/durable-acp-rs
 * 2. Debug build: target/debug/durable-acp-rs
 * 3. DURABLE_ACP_BIN env var (explicit override)
 */
export function findBinary(): string {
  // Env override
  if (process.env.DURABLE_ACP_BIN) {
    if (existsSync(process.env.DURABLE_ACP_BIN)) return process.env.DURABLE_ACP_BIN;
  }

  // Walk from dist/ → ts/packages/server/dist/ → repo root
  const repoRoot = path.resolve(__dirname, "../../../..");

  const releaseBin = path.join(repoRoot, "target/release/durable-acp-rs");
  if (existsSync(releaseBin)) return releaseBin;

  const debugBin = path.join(repoRoot, "target/debug/durable-acp-rs");
  if (existsSync(debugBin)) return debugBin;

  throw new Error(
    "durable-acp-rs binary not found. Run `cargo build` or set DURABLE_ACP_BIN."
  );
}
