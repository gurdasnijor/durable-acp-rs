/**
 * Integration test: TS durable-session ↔ Rust durable streams server.
 *
 * Spawns ds_server (bare durable streams HTTP server), appends
 * STATE-PROTOCOL events, verifies durable-session materializes them.
 *
 * Same assertions as tests/e2e_full_stack.rs but from TypeScript.
 */
import { describe, it, expect, beforeAll, afterAll } from "vitest";
import { spawn, type ChildProcess } from "child_process";
import path from "path";
import { createDurableACPDB } from "../src/collection";
import { durableACPState } from "../src/schema";

const REPO_ROOT = path.resolve(__dirname, "../../../..");
const BINARY = path.join(REPO_ROOT, "target/debug/ds_server");

const PORT = 14200 + Math.floor(Math.random() * 800);
const STATE_STREAM_URL = `http://127.0.0.1:${PORT}/streams/durable-acp-state`;

let server: ChildProcess | null = null;

async function waitForServer(timeoutMs = 10000): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    try {
      const res = await fetch(STATE_STREAM_URL);
      if (res.ok || res.status === 200) return;
    } catch {
      // not ready
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`Server did not start within ${timeoutMs}ms`);
}

async function appendEvent(event: Record<string, unknown>): Promise<void> {
  const res = await fetch(STATE_STREAM_URL, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(event),
  });
  if (!res.ok) {
    throw new Error(`Append failed: ${res.status} ${await res.text()}`);
  }
}

describe("durable-session ↔ Rust durable streams server", () => {
  beforeAll(async () => {
    server = spawn(BINARY, [String(PORT)], {
      stdio: ["pipe", "pipe", "pipe"],
    });
    server.stderr?.on("data", (d: Buffer) => process.stderr.write(`[ds] ${d}`));
    await waitForServer();
  }, 15000);

  afterAll(() => {
    server?.kill("SIGTERM");
  });

  it("connects and preloads empty collections", async () => {
    const controller = new AbortController();
    const db = createDurableACPDB({ stateStreamUrl: STATE_STREAM_URL, signal: controller.signal });
    await db.preload();

    expect(db.collections.connections).toBeDefined();
    expect(db.collections.promptTurns).toBeDefined();
    expect(db.collections.chunks).toBeDefined();
    expect(db.collections.permissions).toBeDefined();
    expect(db.collections.terminals).toBeDefined();

    controller.abort();
    db.close();
  });

  it("materializes connection + prompt turn + chunks from SSE", async () => {
    // Write a connection
    await appendEvent(durableACPState.connections.insert({
      value: {
        logicalConnectionId: "conn-1",
        state: "attached",
        queuePaused: false,
        createdAt: Date.now(),
        updatedAt: Date.now(),
      },
    }));

    // Write a prompt turn
    await appendEvent(durableACPState.promptTurns.insert({
      value: {
        promptTurnId: "turn-1",
        logicalConnectionId: "conn-1",
        sessionId: "session-1",
        requestId: "req-1",
        text: "hello from TS test",
        state: "completed",
        position: 0,
        stopReason: "end_turn",
        startedAt: Date.now(),
        completedAt: Date.now(),
      },
    }));

    // Write text + stop chunks
    await appendEvent(durableACPState.chunks.insert({
      value: {
        chunkId: "chunk-1",
        promptTurnId: "turn-1",
        logicalConnectionId: "conn-1",
        type: "text",
        content: "Hello from Rust!",
        seq: 0,
        createdAt: Date.now(),
      },
    }));

    await appendEvent(durableACPState.chunks.insert({
      value: {
        chunkId: "chunk-2",
        promptTurnId: "turn-1",
        logicalConnectionId: "conn-1",
        type: "stop",
        content: '{"stopReason":"end_turn"}',
        seq: 1,
        createdAt: Date.now(),
      },
    }));

    // Connect durable-session and verify
    const controller = new AbortController();
    const db = createDurableACPDB({ stateStreamUrl: STATE_STREAM_URL, signal: controller.signal });
    await db.preload();

    // Connection
    const connections = [...db.collections.connections.toArray];
    expect(connections.length).toBe(1);
    expect(connections[0].logicalConnectionId).toBe("conn-1");
    expect(connections[0].state).toBe("attached");

    // Prompt turn
    const turns = [...db.collections.promptTurns.toArray];
    expect(turns.length).toBe(1);
    expect(turns[0].text).toBe("hello from TS test");
    expect(turns[0].state).toBe("completed");
    expect(turns[0].stopReason).toBe("end_turn");

    // Chunks
    const chunks = [...db.collections.chunks.toArray];
    expect(chunks.length).toBe(2);
    const textChunk = chunks.find((c) => c.type === "text");
    expect(textChunk).toBeDefined();
    expect(textChunk!.content).toBe("Hello from Rust!");
    const stopChunk = chunks.find((c) => c.type === "stop");
    expect(stopChunk).toBeDefined();

    controller.abort();
    db.close();
  });

  it("materializes updates to existing entities", async () => {
    // Update the connection state
    await appendEvent(durableACPState.connections.update({
      value: {
        logicalConnectionId: "conn-1",
        state: "closed",
        queuePaused: false,
        createdAt: Date.now(),
        updatedAt: Date.now(),
      },
    }));

    const controller = new AbortController();
    const db = createDurableACPDB({ stateStreamUrl: STATE_STREAM_URL, signal: controller.signal });
    await db.preload();

    const conn = [...db.collections.connections.toArray].find((c) => c.logicalConnectionId === "conn-1");
    expect(conn).toBeDefined();
    expect(conn!.state).toBe("closed");

    controller.abort();
    db.close();
  });

  it("materializes permissions", async () => {
    await appendEvent(durableACPState.permissions.insert({
      value: {
        requestId: "perm-1",
        jsonrpcId: 42,
        logicalConnectionId: "conn-1",
        sessionId: "session-1",
        promptTurnId: "turn-1",
        title: "Allow file write?",
        options: [
          { optionId: "allow", name: "Allow", kind: "approve" },
          { optionId: "deny", name: "Deny", kind: "deny" },
        ],
        state: "pending",
        createdAt: Date.now(),
      },
    }));

    const controller = new AbortController();
    const db = createDurableACPDB({ stateStreamUrl: STATE_STREAM_URL, signal: controller.signal });
    await db.preload();

    const perms = [...db.collections.permissions.toArray];
    expect(perms.length).toBe(1);
    expect(perms[0].title).toBe("Allow file write?");
    expect(perms[0].state).toBe("pending");
    expect(perms[0].options).toHaveLength(2);

    controller.abort();
    db.close();
  });
});
