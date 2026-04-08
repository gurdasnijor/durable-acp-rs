/**
 * Start a durable-acp-rs server process.
 *
 * Usage:
 *   const server = await startServer({ agent: "claude-acp" });
 *   // server.acpUrl, server.streamUrl, server.apiUrl
 *   server.kill();
 */

import { spawn, type ChildProcess } from "child_process";
import { findBinary } from "./binary";

export interface ServerConfig {
  /** Agent ID from ACP registry, or { command: string[] } for custom. */
  agent: string | { command: string[] };
  /** Base port. Streams on :port, API on :port+1. Default: 4437. */
  port?: number;
  /** Server name for peer registry. */
  name?: string;
  /** State stream name. Default: "durable-acp-state". */
  stateStream?: string;
  /** Environment variables to pass to the process. */
  env?: Record<string, string>;
}

export interface ServerHandle {
  /** ACP WebSocket URL for prompts/commands. */
  acpUrl: string;
  /** Durable streams SSE URL for state observation. */
  streamUrl: string;
  /** REST API URL for queue management + filesystem. */
  apiUrl: string;
  /** Process ID. */
  pid: number;
  /** Kill the server process. */
  kill(): void;
  /** Promise that resolves when the process exits. */
  exited: Promise<number | null>;
  /** The underlying child process. */
  process: ChildProcess;
}

/**
 * Start a durable-acp-rs server and wait for it to be ready.
 */
export async function startServer(config: ServerConfig): Promise<ServerHandle> {
  const binary = findBinary();
  const port = config.port ?? 4437;
  const name = config.name ?? "default";
  const stateStream = config.stateStream ?? "durable-acp-state";

  const agentArgs = typeof config.agent === "string"
    ? ["npx", `@agentclientprotocol/${config.agent}`]
    : config.agent.command;

  const child = spawn(binary, [
    "--port", String(port),
    "--name", name,
    "--state-stream", stateStream,
    ...agentArgs,
  ], {
    stdio: ["pipe", "pipe", "inherit"],
    env: { ...process.env, ...config.env },
  });

  const exited = new Promise<number | null>((resolve) => {
    child.on("exit", (code) => resolve(code));
  });

  // Wait for the API server to be ready
  const apiPort = port + 1;
  await waitForReady(`http://127.0.0.1:${apiPort}/api/v1/registry`, 10000);

  return {
    acpUrl: `ws://127.0.0.1:${apiPort}/acp`,
    streamUrl: `http://127.0.0.1:${port}/streams/${stateStream}`,
    apiUrl: `http://127.0.0.1:${apiPort}`,
    pid: child.pid!,
    kill: () => child.kill("SIGTERM"),
    exited,
    process: child,
  };
}

async function waitForReady(url: string, timeoutMs: number): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const res = await fetch(url);
      if (res.ok) return;
    } catch {
      // not ready yet
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`Server did not start within ${timeoutMs}ms (${url})`);
}
