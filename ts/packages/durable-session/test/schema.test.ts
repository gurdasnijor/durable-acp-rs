import { describe, it, expect } from "vitest";
import { durableACPState } from "../src/schema";

describe("durableACPState schema", () => {
  const collectionNames = [
    "connections",
    "promptTurns",
    "pendingRequests",
    "permissions",
    "terminals",
    "runtimeInstances",
    "chunks",
  ] as const;

  it("has exactly 7 collections", () => {
    const keys = Object.keys(durableACPState);
    expect(keys.length).toBe(7);
    for (const name of collectionNames) {
      expect(keys).toContain(name);
    }
  });

  it("creates valid insert event for connections", () => {
    const event = durableACPState.connections.insert({
      value: {
        logicalConnectionId: "c1",
        state: "created",
        queuePaused: false,
        createdAt: 1000,
        updatedAt: 1000,
      },
    });
    expect(event.type).toBe("connection");
    expect(event.headers.operation).toBe("insert");
    expect(event.key).toBe("c1");
  });

  it("creates valid insert event for promptTurns", () => {
    const event = durableACPState.promptTurns.insert({
      value: {
        promptTurnId: "t1",
        logicalConnectionId: "c1",
        sessionId: "s1",
        requestId: "r1",
        text: "hello",
        state: "queued",
        position: 0,
        startedAt: 0,
      },
    });
    expect(event.type).toBe("prompt_turn");
    expect(event.headers.operation).toBe("insert");
    expect(event.key).toBe("t1");
  });

  it("creates valid insert event for pendingRequests", () => {
    const event = durableACPState.pendingRequests.insert({
      value: {
        requestId: "req1",
        logicalConnectionId: "c1",
        sessionId: "s1",
        method: "session/prompt",
        direction: "client_to_agent",
        state: "pending",
        createdAt: 1000,
      },
    });
    expect(event.type).toBe("pending_request");
    expect(event.headers.operation).toBe("insert");
    expect(event.key).toBe("req1");
  });

  it("creates valid insert event for permissions", () => {
    const event = durableACPState.permissions.insert({
      value: {
        requestId: "perm1",
        jsonrpcId: 42,
        logicalConnectionId: "c1",
        sessionId: "s1",
        promptTurnId: "t1",
        title: "Allow file write?",
        options: [{ optionId: "allow", name: "Allow", kind: "approve" }],
        state: "pending",
        createdAt: 1000,
      },
    });
    expect(event.type).toBe("permission");
    expect(event.headers.operation).toBe("insert");
    expect(event.key).toBe("perm1");
  });

  it("creates valid insert event for terminals", () => {
    const event = durableACPState.terminals.insert({
      value: {
        terminalId: "term1",
        logicalConnectionId: "c1",
        sessionId: "s1",
        state: "open",
        command: "bash",
        createdAt: 1000,
        updatedAt: 1000,
      },
    });
    expect(event.type).toBe("terminal");
    expect(event.headers.operation).toBe("insert");
    expect(event.key).toBe("term1");
  });

  it("creates valid insert event for runtimeInstances", () => {
    const event = durableACPState.runtimeInstances.insert({
      value: {
        instanceId: "inst1",
        runtimeName: "local",
        status: "running",
        createdAt: 1000,
        updatedAt: 1000,
      },
    });
    expect(event.type).toBe("runtime_instance");
    expect(event.headers.operation).toBe("insert");
    expect(event.key).toBe("inst1");
  });

  it("creates valid insert event for chunks", () => {
    const event = durableACPState.chunks.insert({
      value: {
        chunkId: "chunk1",
        promptTurnId: "t1",
        logicalConnectionId: "c1",
        type: "text",
        content: "Hello world",
        seq: 0,
        createdAt: 1000,
      },
    });
    expect(event.type).toBe("chunk");
    expect(event.headers.operation).toBe("insert");
    expect(event.key).toBe("chunk1");
  });

  it("creates valid update event for promptTurns", () => {
    const event = durableACPState.promptTurns.update({
      value: {
        promptTurnId: "t1",
        logicalConnectionId: "c1",
        sessionId: "s1",
        requestId: "r1",
        state: "active",
        startedAt: Date.now(),
      },
    });
    expect(event.type).toBe("prompt_turn");
    expect(event.headers.operation).toBe("update");
    expect(event.key).toBe("t1");
  });

  it("creates valid update event for connections", () => {
    const event = durableACPState.connections.update({
      value: {
        logicalConnectionId: "c1",
        state: "attached",
        queuePaused: true,
        createdAt: 1000,
        updatedAt: Date.now(),
      },
    });
    expect(event.type).toBe("connection");
    expect(event.headers.operation).toBe("update");
    expect(event.key).toBe("c1");
  });
});
