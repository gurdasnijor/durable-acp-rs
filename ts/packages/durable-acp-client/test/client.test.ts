/**
 * Tests for DurableACPClient.
 *
 * Uses mock DB injection for testing without network.
 */

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { DurableACPClient, createDurableACPClient } from "../src/client";
import {
  createMockDurableACPDB,
  type MockDurableACPDBControllers,
} from "./fixtures/test-helpers";
import type { DurableACPClientOptions } from "../src/types";
import type { DurableACPDB } from "@durable-acp/state";
import type { PromptTurnRow } from "@durable-acp/state";

describe("DurableACPClient", () => {
  const defaultOptions: DurableACPClientOptions = {
    connectionId: "test-conn",
    serverUrl: "http://localhost:4001/api/v1",
    stateStreamUrl: "http://localhost:4437/durable-acp-state",
  };

  let client: DurableACPClient;
  let db: DurableACPDB;

  beforeEach(() => {
    const mock = createMockDurableACPDB();
    db = mock.db;
    client = new DurableACPClient({
      ...defaultOptions,
      _db: db,
    });
  });

  afterEach(() => {
    client.dispose();
    vi.clearAllMocks();
  });

  // ========================================================================
  // Construction
  // ========================================================================

  describe("construction", () => {
    it("creates with correct connectionId", () => {
      expect(client.connectionId).toBe("test-conn");
    });

    it("creates with injected _db (testing)", () => {
      expect(client.collections).toBeDefined();
    });

    it("generates unique client via createDurableACPClient factory", () => {
      const factoryClient = createDurableACPClient({
        ...defaultOptions,
        _db: db,
      });
      expect(factoryClient).toBeInstanceOf(DurableACPClient);
      expect(factoryClient.connectionId).toBe("test-conn");
      factoryClient.dispose();
    });
  });

  // ========================================================================
  // Pre-connect State
  // ========================================================================

  describe("pre-connect state", () => {
    it("should not be loading before connect", () => {
      expect(client.isLoading).toBe(false);
    });

    it("has no error initially", () => {
      expect(client.error).toBeUndefined();
    });

    it("has collections available immediately", () => {
      expect(client.collections).toBeDefined();
      expect(client.collections.connections).toBeDefined();
      expect(client.collections.promptTurns).toBeDefined();
      expect(client.collections.permissions).toBeDefined();
      expect(client.collections.chunks).toBeDefined();
      expect(client.collections.queuedTurns).toBeDefined();
      expect(client.collections.activeTurns).toBeDefined();
      expect(client.collections.pendingPermissions).toBeDefined();
    });
  });

  // ========================================================================
  // Connect/Disconnect
  // ========================================================================

  describe("connect/disconnect", () => {
    it("connect() preloads StreamDB", async () => {
      await client.connect();
      expect(typeof client.collections.connections.size).toBe("number");
    });

    it("connect() is idempotent", async () => {
      await client.connect();
      await client.connect();
    });

    it("dispose() is idempotent", () => {
      client.dispose();
      client.dispose();
      expect(client.isDisposed).toBe(true);
    });

    it("dispose sets isDisposed", () => {
      expect(client.isDisposed).toBe(false);
      client.dispose();
      expect(client.isDisposed).toBe(true);
    });
  });

  // ========================================================================
  // Commands require connect
  // ========================================================================

  describe("commands require connect", () => {
    it("prompt throws before connect", async () => {
      await expect(client.prompt("hello")).rejects.toThrow(
        "Client not connected",
      );
    });

    it("cancel throws before connect", async () => {
      await expect(client.cancel()).rejects.toThrow("Client not connected");
    });

    it("pause throws before connect", async () => {
      await expect(client.pause()).rejects.toThrow("Client not connected");
    });

    it("resume throws before connect", async () => {
      await expect(client.resume()).rejects.toThrow("Client not connected");
    });

    it("resolvePermission throws before connect", async () => {
      await expect(client.resolvePermission("r1", "opt1")).rejects.toThrow(
        "Client not connected",
      );
    });
  });

  // ========================================================================
  // Commands
  // ========================================================================

  describe("commands", () => {
    beforeEach(async () => {
      global.fetch = vi.fn().mockResolvedValue({
        ok: true,
        status: 200,
      });
      await client.connect();
    });

    it("prompt() POSTs to serverUrl", async () => {
      await client.prompt("hello");

      expect(fetch).toHaveBeenCalledWith(
        "http://localhost:4001/api/v1/connections/test-conn/prompt",
        expect.objectContaining({
          method: "POST",
          body: expect.stringContaining('"text":"hello"'),
        }),
      );
    });

    it("prompt() optimistically inserts queued turn", async () => {
      const insertedTurns: PromptTurnRow[] = [];
      client.onPromptTurnChanges((changes) => {
        for (const c of changes) {
          if (c.type === "insert") insertedTurns.push(c.value);
        }
      });

      const promptTurnId = await client.prompt("hello world");
      expect(promptTurnId).toBeDefined();
      expect(typeof promptTurnId).toBe("string");

      const inserted = insertedTurns.find(
        (t) => t.promptTurnId === promptTurnId,
      );
      expect(inserted).toBeDefined();
      expect(inserted?.state).toBe("queued");
      expect(inserted?.text).toBe("hello world");
    });

    it("cancel() POSTs to serverUrl", async () => {
      await client.cancel();
      expect(fetch).toHaveBeenCalledWith(
        "http://localhost:4001/api/v1/connections/test-conn/cancel",
        expect.objectContaining({ method: "POST" }),
      );
    });

    it("pause() POSTs to serverUrl", async () => {
      await client.pause();
      expect(fetch).toHaveBeenCalledWith(
        "http://localhost:4001/api/v1/connections/test-conn/queue/pause",
        expect.objectContaining({ method: "POST" }),
      );
    });

    it("resume() POSTs to serverUrl", async () => {
      await client.resume();
      expect(fetch).toHaveBeenCalledWith(
        "http://localhost:4001/api/v1/connections/test-conn/queue/resume",
        expect.objectContaining({ method: "POST" }),
      );
    });

    it("reorder() POSTs with turnIds array", async () => {
      await client.reorder(["t2", "t1"]);
      expect(fetch).toHaveBeenCalledWith(
        "http://localhost:4001/api/v1/connections/test-conn/queue",
        expect.objectContaining({
          method: "POST",
          body: expect.stringContaining('"turnIds"'),
        }),
      );
    });
  });

  // ========================================================================
  // Callbacks
  // ========================================================================

  describe("callbacks", () => {
    it("onError is stored in options", () => {
      const onError = vi.fn();
      const clientWithCb = new DurableACPClient({
        ...defaultOptions,
        onError,
        _db: db,
      });
      expect(clientWithCb["_options"].onError).toBe(onError);
      clientWithCb.dispose();
    });
  });

  // ========================================================================
  // Typed Accessors
  // ========================================================================

  describe("typed accessors", () => {
    beforeEach(async () => {
      await client.connect();
    });

    it("getConnections returns array", () => {
      expect(Array.isArray(client.getConnections())).toBe(true);
    });

    it("getPromptTurns returns array", () => {
      expect(Array.isArray(client.getPromptTurns())).toBe(true);
    });

    it("getPermissions returns array", () => {
      expect(Array.isArray(client.getPermissions())).toBe(true);
    });

    it("getConnection returns undefined for missing id", () => {
      expect(client.getConnection("nonexistent")).toBeUndefined();
    });

    it("getPromptTurn returns undefined for missing id", () => {
      expect(client.getPromptTurn("nonexistent")).toBeUndefined();
    });
  });

  // ========================================================================
  // isLoading
  // ========================================================================

  describe("isLoading", () => {
    beforeEach(async () => {
      await client.connect();
    });

    it("derives from activeTurns collection size", () => {
      expect(client.isLoading).toBe(false);
    });
  });
});
