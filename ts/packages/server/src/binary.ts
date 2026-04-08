/**
 * Platform binary resolution for durable-acp-rs.
 *
 * Resolution order:
 * 1. DURABLE_ACP_BIN env var (explicit override)
 * 2. Whichever of debug/release is newer (most recently built)
 * 3. Whichever exists
 */

import path from "path";
import { existsSync, statSync } from "fs";
import { fileURLToPath } from "url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export function findBinary(): string {
  // 1. Env override
  if (process.env.DURABLE_ACP_BIN) {
    if (existsSync(process.env.DURABLE_ACP_BIN)) return process.env.DURABLE_ACP_BIN;
  }

  // Walk from src/ → ts/packages/server/src/ → repo root
  const repoRoot = path.resolve(__dirname, "../../../..");

  const releaseBin = path.join(repoRoot, "target/release/durable-acp-rs");
  const debugBin = path.join(repoRoot, "target/debug/durable-acp-rs");

  const releaseExists = existsSync(releaseBin);
  const debugExists = existsSync(debugBin);

  // 2. Both exist — pick the newer one
  if (releaseExists && debugExists) {
    const releaseMtime = statSync(releaseBin).mtimeMs;
    const debugMtime = statSync(debugBin).mtimeMs;
    return debugMtime >= releaseMtime ? debugBin : releaseBin;
  }

  // 3. Whichever exists
  if (debugExists) return debugBin;
  if (releaseExists) return releaseBin;

  throw new Error(
    "durable-acp-rs binary not found. Run `cargo build` or set DURABLE_ACP_BIN."
  );
}
