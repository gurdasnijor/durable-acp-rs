/**
 * Integration test: TS client ↔ Rust durable streams server.
 *
 * Spawns a real durable-acp-rs conductor, writes STATE-PROTOCOL events,
 * and verifies @durable-acp/state materializes them into collections.
 */
import { describe, it, expect, beforeAll, afterAll } from "vitest";
import { execSync, spawn, type ChildProcess } from "child_process";
import path from "path";
import { createDurableACPDB, type DurableACPDB } from "../src/collection";
import { durableACPState } from "../src/schema";

// Resolve binary relative to this test file (ts/packages/durable-acp-state/test/ → repo root)
const REPO_ROOT = path.resolve(__dirname, "../../../..");
const BINARY = path.join(REPO_ROOT, "target/debug/durable-acp-rs");

// Use random ports to avoid conflicts
const STREAMS_PORT = 14000 + Math.floor(Math.random() * 1000);
const API_PORT = STREAMS_PORT + 1;

const STATE_STREAM_URL = `http://127.0.0.1:${STREAMS_PORT}/streams/durable-acp-state`;

let conductor: ChildProcess | null = null;

async function waitForServer(url: string, timeoutMs = 10000): Promise<void> {
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
  throw new Error(`Server at ${url} did not start within ${timeoutMs}ms`);
}

async function appendEvent(event: Record<string, unknown>): Promise<void> {
  const res = await fetch(STATE_STREAM_URL, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(event),
  });
  if (!res.ok) {
    throw new Error(`Failed to append event: ${res.status} ${await res.text()}`);
  }
}

describe("durable-acp-state integration", () => {
  beforeAll(async () => {
    // Build the binary if needed
    try {
      execSync(`ls ${BINARY}`, { stdio: "ignore" });
    } catch {
      execSync("cargo build", { cwd: "../../..", stdio: "inherit" });
    }

    // Start conductor with a dummy agent (echo — will fail but the streams server starts)
    conductor = spawn(BINARY, [
      "--port", String(STREAMS_PORT),
      "--name", "integration-test",
      "cat",  // cat blocks on stdin indefinitely — keeps the conductor alive
    ], {
      stdio: ["pipe", "pipe", "pipe"],
    });

    // Log stderr for debugging
    conductor.stderr?.on("data", (data: Buffer) => {
      process.stderr.write(`[conductor] ${data}`);
    });

    // Wait for durable streams server to be ready
    await waitForServer(`http://127.0.0.1:${API_PORT}/api/v1/registry`);
  }, 30000);

  afterAll(() => {
    conductor?.kill("SIGTERM");
  });

  it("connects to Rust durable streams server and preloads", async () => {
    const db = createDurableACPDB({ stateStreamUrl: STATE_STREAM_URL });
    await db.preload();

    // Collections exist and are queryable (may be empty if conductor agent exited)
    expect(db.collections.connections).toBeDefined();
    expect(db.collections.promptTurns).toBeDefined();
    expect(db.collections.chunks).toBeDefined();
    expect(db.collections.permissions).toBeDefined();

    db.close();
  });

  it("materializes manually appended STATE-PROTOCOL events", async () => {
    const controller = new AbortController();
    const db = createDurableACPDB({
      stateStreamUrl: STATE_STREAM_URL,
      signal: controller.signal,
    });
    await db.preload();

    // Write a prompt turn event via HTTP (same format the Rust conductor uses)
    const turnEvent = durableACPState.promptTurns.insert({
      value: {
        promptTurnId: "test-turn-1",
        logicalConnectionId: "test-conn",
        sessionId: "test-session",
        requestId: "test-req",
        text: "hello from integration test",
        state: "queued",
        position: 0,
        startedAt: Date.now(),
      },
    });
    await appendEvent(turnEvent);

    // Write a chunk event
    const chunkEvent = durableACPState.chunks.insert({
      value: {
        chunkId: "test-chunk-1",
        promptTurnId: "test-turn-1",
        logicalConnectionId: "test-conn",
        type: "text",
        content: "Hello from Rust!",
        seq: 0,
        createdAt: Date.now(),
      },
    });
    await appendEvent(chunkEvent);

    // Wait for SSE to deliver the events
    await new Promise((r) => setTimeout(r, 500));

    // Verify materialized collections
    const turns = [...db.collections.promptTurns.toArray];
    const turn = turns.find((t) => t.promptTurnId === "test-turn-1");
    expect(turn).toBeDefined();
    expect(turn!.text).toBe("hello from integration test");
    expect(turn!.state).toBe("queued");

    const chunks = [...db.collections.chunks.toArray];
    const chunk = chunks.find((c) => c.chunkId === "test-chunk-1");
    expect(chunk).toBeDefined();
    expect(chunk!.content).toBe("Hello from Rust!");
    expect(chunk!.type).toBe("text");

    controller.abort();
    db.close();
  });

  it("materializes updates to existing entities", async () => {
    const controller = new AbortController();
    const db = createDurableACPDB({
      stateStreamUrl: STATE_STREAM_URL,
      signal: controller.signal,
    });
    await db.preload();

    // Update the turn we created in the previous test
    const updateEvent = durableACPState.promptTurns.update({
      value: {
        promptTurnId: "test-turn-1",
        logicalConnectionId: "test-conn",
        sessionId: "test-session",
        requestId: "test-req",
        state: "active",
        startedAt: Date.now(),
      },
    });
    await appendEvent(updateEvent);

    await new Promise((r) => setTimeout(r, 500));

    const turns = [...db.collections.promptTurns.toArray];
    const turn = turns.find((t) => t.promptTurnId === "test-turn-1");
    expect(turn).toBeDefined();
    expect(turn!.state).toBe("active");

    controller.abort();
    db.close();
  });
});
