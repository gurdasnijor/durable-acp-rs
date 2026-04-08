# Importing durable-acp-rs into Flamecast

> Goal: `pnpm add @durable-acp/server` — that's it.

## Consumer Experience

```typescript
import { createConductor } from "@durable-acp/server";

const conductor = await createConductor({
  agent: "claude-acp",   // or { command: ["npx", "my-agent"] }
  port: 4437,
});

// conductor.acpUrl     → ws://127.0.0.1:4438/acp
// conductor.streamUrl  → http://127.0.0.1:4437/streams/durable-acp-state
// conductor.apiUrl     → http://127.0.0.1:4438
// conductor.kill()
```

Flamecast doesn't know about Rust, cargo, or binary paths. The npm
package includes the pre-built binary for the current platform.

## npm Package Structure

Same pattern as `esbuild`, `turbo`, `@biomejs/biome`:

```
@durable-acp/server                  ← main package (JS wrapper)
@durable-acp/server-darwin-arm64     ← binary for Apple Silicon
@durable-acp/server-darwin-x64       ← binary for Intel Mac
@durable-acp/server-linux-x64        ← binary for Linux x64
@durable-acp/server-linux-arm64      ← binary for Linux ARM
```

### Main package (`@durable-acp/server`)

```json
{
  "name": "@durable-acp/server",
  "version": "0.1.0",
  "main": "./dist/index.js",
  "types": "./dist/index.d.ts",
  "dependencies": {
    "@durable-acp/transport": "workspace:*",
    "durable-session": "workspace:*"
  },
  "optionalDependencies": {
    "@durable-acp/server-darwin-arm64": "0.1.0",
    "@durable-acp/server-darwin-x64": "0.1.0",
    "@durable-acp/server-linux-x64": "0.1.0",
    "@durable-acp/server-linux-arm64": "0.1.0"
  }
}
```

### Platform packages (one per target)

```json
{
  "name": "@durable-acp/server-darwin-arm64",
  "version": "0.1.0",
  "os": ["darwin"],
  "cpu": ["arm64"],
  "bin": {
    "durable-acp-rs": "./bin/durable-acp-rs"
  }
}
```

Each contains only the compiled binary (~10MB). npm/pnpm automatically
installs only the matching platform package via `optionalDependencies` + `os`/`cpu` fields.

## Package Contents

### `@durable-acp/server` — the main package

```
ts/packages/server/
  src/
    index.ts          ← re-exports everything
    conductor.ts      ← createConductor(), ConductorHandle
    binary.ts         ← findBinary() — resolves platform binary path
  package.json
```

```typescript
// src/conductor.ts
import { spawn, ChildProcess } from "child_process";
import { findBinary } from "./binary";

export interface ConductorConfig {
  /** Agent ID from ACP registry, or { command: string[] } for custom. */
  agent: string | { command: string[] };
  /** Base port. Streams on :port, API on :port+1. Default: 4437. */
  port?: number;
  /** Conductor name for peer registry. */
  name?: string;
}

export interface ConductorHandle {
  /** ACP WebSocket URL for prompts/commands. */
  acpUrl: string;
  /** Durable streams SSE URL for state observation. */
  streamUrl: string;
  /** REST API URL for queue management. */
  apiUrl: string;
  /** Process ID. */
  pid: number;
  /** Kill the conductor process. */
  kill(): void;
  /** The underlying child process. */
  process: ChildProcess;
}

export async function createConductor(config: ConductorConfig): Promise<ConductorHandle> {
  const binary = findBinary();
  const port = config.port ?? 4437;
  const name = config.name ?? "default";

  const agentArgs = typeof config.agent === "string"
    ? ["npx", `@agentclientprotocol/${config.agent}`]
    : config.agent.command;

  const child = spawn(binary, [
    "--port", String(port),
    "--name", name,
    ...agentArgs,
  ], {
    stdio: ["pipe", "pipe", "inherit"],
  });

  // Wait for the conductor to be ready (streams server listening)
  await waitForPort(port, 5000);

  return {
    acpUrl: `ws://127.0.0.1:${port + 1}/acp`,
    streamUrl: `http://127.0.0.1:${port}/streams/durable-acp-state`,
    apiUrl: `http://127.0.0.1:${port + 1}`,
    pid: child.pid!,
    kill: () => child.kill(),
    process: child,
  };
}
```

```typescript
// src/binary.ts
import path from "path";
import { existsSync } from "fs";

const PLATFORM_PACKAGES: Record<string, string> = {
  "darwin-arm64": "@durable-acp/server-darwin-arm64",
  "darwin-x64": "@durable-acp/server-darwin-x64",
  "linux-x64": "@durable-acp/server-linux-x64",
  "linux-arm64": "@durable-acp/server-linux-arm64",
};

export function findBinary(): string {
  const key = `${process.platform}-${process.arch}`;
  const pkg = PLATFORM_PACKAGES[key];
  if (!pkg) throw new Error(`Unsupported platform: ${key}`);

  // Try to find the binary in the platform package
  try {
    const pkgDir = path.dirname(require.resolve(`${pkg}/package.json`));
    const bin = path.join(pkgDir, "bin", "durable-acp-rs");
    if (existsSync(bin)) return bin;
  } catch {}

  // Fallback: check if cargo-built binary exists locally
  const localBin = path.resolve(__dirname, "../../../target/release/durable-acp-rs");
  if (existsSync(localBin)) return localBin;

  throw new Error(
    `durable-acp-rs binary not found. Install ${pkg} or run \`cargo build --release\`.`
  );
}
```

```typescript
// src/index.ts
export { createConductor, type ConductorConfig, type ConductorHandle } from "./conductor";
export { findBinary } from "./binary";

// Re-export transport and state for convenience
export { connectWs, type AcpConnection, type AcpClientHandlers } from "@durable-acp/transport";
export { createDurableACPDB, type DurableACPDB } from "durable-session";
```

## GitHub Actions: Build + Publish

```yaml
# .github/workflows/release.yml
name: Release
on:
  push:
    tags: ["v*"]

jobs:
  build:
    strategy:
      matrix:
        include:
          - os: macos-latest
            target: aarch64-apple-darwin
            npm-pkg: "@durable-acp/server-darwin-arm64"
          - os: macos-13
            target: x86_64-apple-darwin
            npm-pkg: "@durable-acp/server-darwin-x64"
          - os: ubuntu-latest
            target: x86_64-unknown-linux-gnu
            npm-pkg: "@durable-acp/server-linux-x64"
          - os: ubuntu-latest
            target: aarch64-unknown-linux-gnu
            npm-pkg: "@durable-acp/server-linux-arm64"
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          targets: ${{ matrix.target }}
      - run: cargo build --release --target ${{ matrix.target }}
      - run: |
          mkdir -p npm/${{ matrix.npm-pkg }}/bin
          cp target/${{ matrix.target }}/release/durable-acp-rs npm/${{ matrix.npm-pkg }}/bin/
      - uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.npm-pkg }}
          path: npm/${{ matrix.npm-pkg }}

  publish:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/download-artifact@v4
      - uses: pnpm/action-setup@v4
      - run: |
          # Publish each platform package
          for pkg in server-darwin-arm64 server-darwin-x64 server-linux-x64 server-linux-arm64; do
            cd "@durable-acp/$pkg"
            npm publish --access public
            cd ..
          done
          # Publish main package
          cd ts/packages/server && pnpm publish --access public
```

## Flamecast Integration

After `pnpm add @durable-acp/server`:

```typescript
// Flamecast SessionService
import { createConductor, connectWs, createDurableACPDB } from "@durable-acp/server";

class DurableAcpSessionService {
  async startSession(agentId: string) {
    // 1. Spawn conductor (binary auto-resolved per platform)
    const conductor = await createConductor({ agent: agentId });

    // 2. Connect ACP client for prompts
    const acp = await connectWs(conductor.acpUrl, {
      onSessionUpdate: (n) => this.emit("update", n),
    });

    // 3. Subscribe to state for reactive UI
    const db = createDurableACPDB({ stateStreamUrl: conductor.streamUrl });
    await db.preload();

    return { conductor, acp, db };
  }
}
```

### What Flamecast deletes

| Component | Replacement |
|---|---|
| `FlamecastStorage` (PGLite) | `createDurableACPDB()` → TanStack DB collections |
| `@flamecast/psql` | Delete entirely |
| Custom event bus | `db.collections.*.subscribeChanges()` |
| Session metadata tables | `ConnectionRow` + `PromptTurnRow` in stream |
| AcpBridge / runtime-bridge | `connectWs()` from `@durable-acp/transport` |
| Permission brokering | `DurableStateProxy` handles automatically |
| Custom WebSocket server | `/acp` endpoint on conductor |

### What Flamecast gains

- **`pnpm add` and go** — no Rust toolchain needed
- **MCP peering** — agents discover and message each other automatically
- **Durable sessions** — replay from any stream offset
- **Zero database** — FileStorage replaces Postgres
- **Webhook integration** — coalesced events with HMAC signing

## Local Development (with Rust toolchain)

For contributors who have Rust installed, the binary fallback in
`findBinary()` checks `target/release/durable-acp-rs`. So:

```bash
cargo build --release
pnpm dev  # uses local binary, no npm platform package needed
```

## Workspace Layout

```
durable-acp-rs/
  Cargo.toml                    ← Rust crate
  src/                          ← Rust source
  ts/
    packages/
      server/                   ← @durable-acp/server (main package)
      acp-transport/            ← @durable-acp/transport (WebSocket adapter)
      durable-session/          ← durable-session (StreamDB + state)
  npm/
    server-darwin-arm64/        ← platform binary packages (built by CI)
    server-darwin-x64/
    server-linux-x64/
    server-linux-arm64/
```
