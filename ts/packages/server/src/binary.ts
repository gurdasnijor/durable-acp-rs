/**
 * Platform binary resolution for durable-acp-rs.
 *
 * Same pattern as esbuild, turbo, @biomejs/biome:
 * - Try platform-specific npm package first (@durable-acp/server-darwin-arm64)
 * - Fall back to locally built cargo binary (target/release/durable-acp-rs)
 */

import path from "path";
import { existsSync } from "fs";
import { createRequire } from "module";

const require = createRequire(import.meta.url);

const PLATFORM_PACKAGES: Record<string, string> = {
  "darwin-arm64": "@durable-acp/server-darwin-arm64",
  "darwin-x64": "@durable-acp/server-darwin-x64",
  "linux-x64": "@durable-acp/server-linux-x64",
  "linux-arm64": "@durable-acp/server-linux-arm64",
};

/**
 * Find the durable-acp-rs binary for the current platform.
 *
 * Resolution order:
 * 1. Platform npm package (installed via optionalDependencies)
 * 2. Local cargo build (target/release/durable-acp-rs)
 * 3. Local cargo debug build (target/debug/durable-acp-rs)
 */
export function findBinary(): string {
  const key = `${process.platform}-${process.arch}`;
  const pkg = PLATFORM_PACKAGES[key];

  // 1. Try platform npm package
  if (pkg) {
    try {
      const pkgDir = path.dirname(require.resolve(`${pkg}/package.json`));
      const bin = path.join(pkgDir, "bin", "durable-acp-rs");
      if (existsSync(bin)) return bin;
    } catch {
      // Package not installed — fall through to local binary
    }
  }

  // 2. Try local cargo release build (walk up from this package to repo root)
  const repoRoot = path.resolve(__dirname, "../../../..");
  const releaseBin = path.join(repoRoot, "target/release/durable-acp-rs");
  if (existsSync(releaseBin)) return releaseBin;

  // 3. Try local cargo debug build
  const debugBin = path.join(repoRoot, "target/debug/durable-acp-rs");
  if (existsSync(debugBin)) return debugBin;

  throw new Error(
    `durable-acp-rs binary not found for ${key}. ` +
    `Install ${pkg ?? "a platform package"} or run \`cargo build --release\`.`
  );
}
