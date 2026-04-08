/**
 * React bindings for @durable-acp/server.
 *
 * Usage:
 *   import { startServer } from "@durable-acp/server";
 *   import { DurableAcpProvider, useSession } from "@durable-acp/server/react";
 *
 *   const server = await startServer({ agent: "claude-acp" });
 *   <DurableAcpProvider server={server}><App /></DurableAcpProvider>
 */

import { createContext, useContext, useEffect, useMemo, useRef, useState } from "react";
import {
  ClientSideConnection,
  type Client,
  type Agent,
  type SessionNotification,
  type RequestPermissionRequest,
  type RequestPermissionResponse,
} from "@agentclientprotocol/sdk";
import { fromWebSocket } from "@durable-acp/transport";
import { createDurableACPDB, type DurableACPDB, type DurableACPCollections } from "durable-session";
import type { ServerHandle } from "./spawn";

// ============================================================================
// Context
// ============================================================================

interface DurableAcpContextValue {
  /** The server handle with endpoint URLs. */
  server: ServerHandle;
  /** The materialized durable state database. */
  db: DurableACPDB;
}

const DurableAcpContext = createContext<DurableAcpContextValue | null>(null);

// ============================================================================
// Provider
// ============================================================================

export interface DurableAcpProviderProps {
  /** Server handle from startServer(). */
  server: ServerHandle;
  children: React.ReactNode;
}

/**
 * Provides durable ACP context to the React tree.
 *
 * Creates a DurableACPDB (durable-session) from the server's stream URL.
 * Components use useSession() to get an ACP connection + reactive state.
 */
export function DurableAcpProvider({ server, children }: DurableAcpProviderProps) {
  const db = useMemo(
    () => createDurableACPDB({ stateStreamUrl: server.streamUrl }),
    [server.streamUrl],
  );

  // Preload on mount
  useEffect(() => {
    db.preload();
    return () => db.close();
  }, [db]);

  const value = useMemo(() => ({ server, db }), [server, db]);

  return (
    <DurableAcpContext.Provider value={value}>
      {children}
    </DurableAcpContext.Provider>
  );
}

// ============================================================================
// Hooks
// ============================================================================

function useDurableAcp(): DurableAcpContextValue {
  const ctx = useContext(DurableAcpContext);
  if (!ctx) throw new Error("useDurableAcp must be used within <DurableAcpProvider>");
  return ctx;
}

/**
 * Access the durable state collections (reactive, auto-synced via SSE).
 *
 * This is the out-of-band observer — no ACP connection needed.
 */
export function useCollections(): DurableACPCollections {
  return useDurableAcp().db.collections;
}

/**
 * Access the raw DurableACPDB instance.
 */
export function useDb(): DurableACPDB {
  return useDurableAcp().db;
}

/**
 * Access the server handle (URLs, kill, process).
 */
export function useServer(): ServerHandle {
  return useDurableAcp().server;
}

// ============================================================================
// ACP Session Hook
// ============================================================================

export interface UseSessionOptions {
  /** Called on each session update (text chunks, tool calls, etc.) */
  onSessionUpdate?: (notification: SessionNotification) => void;
  /** Handle permission requests. Default: auto-approve first option. */
  onPermissionRequest?: (
    request: RequestPermissionRequest,
  ) => Promise<RequestPermissionResponse>;
  /** Working directory for the session. */
  cwd?: string;
}

export interface SessionState {
  /** The ACP connection (standard @agentclientprotocol/sdk). */
  connection: ClientSideConnection | null;
  /** The session ID from the conductor. */
  sessionId: string | null;
  /** Whether the session is connected and initialized. */
  isReady: boolean;
  /** Connection error, if any. */
  error: Error | null;
  /** Send a text prompt. */
  prompt: (text: string) => Promise<void>;
  /** Cancel the current prompt turn. */
  cancel: () => Promise<void>;
  /** Disconnect the ACP session. */
  disconnect: () => void;
}

/**
 * Connect to the conductor as a standard ACP client.
 *
 * Creates a ClientSideConnection (from @agentclientprotocol/sdk) over
 * WebSocket, initializes the protocol, and creates a session.
 *
 * This is the in-band connection — prompts, cancel, permissions all go
 * through the ACP protocol. State observation comes from useCollections().
 */
export function useSession(options?: UseSessionOptions): SessionState {
  const { server } = useDurableAcp();
  const [connection, setConnection] = useState<ClientSideConnection | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const streamRef = useRef<ReturnType<typeof fromWebSocket> | null>(null);

  // Stable refs for callbacks so they don't cause reconnects
  const optionsRef = useRef(options);
  optionsRef.current = options;

  useEffect(() => {
    let disposed = false;

    async function connect() {
      try {
        const stream = fromWebSocket(server.acpUrl);
        streamRef.current = stream;

        const conn = new ClientSideConnection(
          (_agent: Agent): Client => ({
            requestPermission: async (params) => {
              if (optionsRef.current?.onPermissionRequest) {
                return optionsRef.current.onPermissionRequest(params);
              }
              // Default: auto-approve first option
              const option = params.options?.[0];
              return {
                outcome: option
                  ? { type: "selected", optionId: option.optionId }
                  : { type: "cancelled" },
              } as unknown as RequestPermissionResponse;
            },
            sessionUpdate: async (params) => {
              optionsRef.current?.onSessionUpdate?.(params);
            },
          }),
          stream,
        );

        if (disposed) { stream.close(); return; }

        // Initialize
        await conn.initialize({
          protocolVersion: "2025-03-26",
          clientInfo: { name: "flamecast", version: "0.1.0" },
          capabilities: {},
        } as any);

        if (disposed) { stream.close(); return; }

        // Create session
        const cwd = optionsRef.current?.cwd ?? "/";
        const sessionResponse = await conn.newSession({ cwd } as any);

        if (disposed) { stream.close(); return; }

        setConnection(conn);
        setSessionId(sessionResponse.sessionId);
      } catch (e) {
        if (!disposed) {
          setError(e instanceof Error ? e : new Error(String(e)));
        }
      }
    }

    connect();

    return () => {
      disposed = true;
      streamRef.current?.close();
      streamRef.current = null;
      setConnection(null);
      setSessionId(null);
    };
  }, [server.acpUrl]);

  const prompt = async (text: string) => {
    if (!connection || !sessionId) throw new Error("Session not ready");
    await connection.prompt({
      sessionId,
      messages: [{ role: "user", content: { type: "text", text } }],
    } as any);
  };

  const cancel = async () => {
    if (!connection || !sessionId) throw new Error("Session not ready");
    await connection.cancel({ sessionId } as any);
  };

  const disconnect = () => {
    streamRef.current?.close();
    streamRef.current = null;
    setConnection(null);
    setSessionId(null);
  };

  return {
    connection,
    sessionId,
    isReady: connection !== null && sessionId !== null,
    error,
    prompt,
    cancel,
    disconnect,
  };
}
