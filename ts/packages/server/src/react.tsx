/**
 * React bindings for @durable-acp/server.
 *
 * Usage:
 *   import { DurableAcpProvider, useSession, useCollections } from "@durable-acp/server/react";
 *
 *   <DurableAcpProvider>          // relative paths, works with Vite proxy
 *   <DurableAcpProvider basePath="http://host:4437">  // explicit base
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

// ============================================================================
// URL resolution
// ============================================================================

interface ResolvedEndpoints {
  acpUrl: string;
  streamUrl: string;
  apiUrl: string;
}

function resolveEndpoints(basePath?: string): ResolvedEndpoints {
  if (basePath) {
    // Explicit base: basePath is the streams port, API is port+1
    const base = basePath.replace(/\/$/, "");
    const url = new URL(base);
    const streamsPort = parseInt(url.port || "4437", 10);
    const apiPort = streamsPort + 1;
    const wsProtocol = url.protocol === "https:" ? "wss:" : "ws:";
    return {
      acpUrl: `${wsProtocol}//${url.hostname}:${apiPort}/acp`,
      streamUrl: `${base}/streams/durable-acp-state`,
      apiUrl: `${url.protocol}//${url.hostname}:${apiPort}`,
    };
  }

  // Relative paths — works behind Vite proxy or reverse proxy
  if (typeof window !== "undefined") {
    const wsProtocol = location.protocol === "https:" ? "wss:" : "ws:";
    return {
      acpUrl: `${wsProtocol}//${location.host}/acp`,
      streamUrl: `${location.origin}/streams/durable-acp-state`,
      apiUrl: location.origin,
    };
  }

  // SSR fallback
  return {
    acpUrl: "ws://127.0.0.1:4438/acp",
    streamUrl: "http://127.0.0.1:4437/streams/durable-acp-state",
    apiUrl: "http://127.0.0.1:4438",
  };
}

// ============================================================================
// Context
// ============================================================================

interface DurableAcpContextValue {
  endpoints: ResolvedEndpoints;
  db: DurableACPDB;
  session: SessionState;
}

const DurableAcpContext = createContext<DurableAcpContextValue | null>(null);

// ============================================================================
// Provider
// ============================================================================

export interface DurableAcpProviderProps {
  /**
   * Base URL of the conductor's streams port (e.g., "http://host:4437").
   * If omitted, uses relative paths (works with Vite proxy or reverse proxy).
   */
  basePath?: string;
  /** ACP session options (permission handler, session update handler, cwd). */
  sessionOptions?: UseSessionOptions;
  children: React.ReactNode;
}

export function DurableAcpProvider({ basePath, sessionOptions, children }: DurableAcpProviderProps) {
  const endpoints = useMemo(() => resolveEndpoints(basePath), [basePath]);

  const db = useMemo(
    () => createDurableACPDB({ stateStreamUrl: endpoints.streamUrl }),
    [endpoints.streamUrl],
  );

  useEffect(() => {
    db.preload();
    return () => db.close();
  }, [db]);

  // -- Singleton ACP connection, created once in the provider --
  const [connection, setConnection] = useState<ClientSideConnection | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const streamRef = useRef<ReturnType<typeof fromWebSocket> | null>(null);
  const optionsRef = useRef(sessionOptions);
  optionsRef.current = sessionOptions;

  useEffect(() => {
    let disposed = false;

    async function connect() {
      try {
        const stream = fromWebSocket(endpoints.acpUrl);
        streamRef.current = stream;

        const conn = new ClientSideConnection(
          (_agent: Agent): Client => ({
            requestPermission: async (params) => {
              if (optionsRef.current?.onPermissionRequest) {
                return optionsRef.current.onPermissionRequest(params);
              }
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

        await conn.initialize({
          protocolVersion: "2025-03-26",
          clientInfo: { name: "durable-acp-client", version: "0.1.0" },
          capabilities: {},
        } as any);

        if (disposed) { stream.close(); return; }

        const cwd = optionsRef.current?.cwd ?? "/";
        const sessionResponse = await conn.newSession({ cwd, mcpServers: [] } as any);

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
  }, [endpoints.acpUrl]);

  const prompt = useMemo(() => async (text: string) => {
    if (!connection || !sessionId) throw new Error("Session not ready");
    await connection.prompt({
      sessionId,
      messages: [{ role: "user", content: { type: "text", text } }],
    } as any);
  }, [connection, sessionId]);

  const cancel = useMemo(() => async () => {
    if (!connection || !sessionId) throw new Error("Session not ready");
    await connection.cancel({ sessionId } as any);
  }, [connection, sessionId]);

  const disconnect = useMemo(() => () => {
    streamRef.current?.close();
    streamRef.current = null;
    setConnection(null);
    setSessionId(null);
  }, []);

  const session: SessionState = useMemo(() => ({
    connection, sessionId, isReady: !!connection && !!sessionId, error, prompt, cancel, disconnect,
  }), [connection, sessionId, error, prompt, cancel, disconnect]);

  const value = useMemo(() => ({ endpoints, db, session }), [endpoints, db, session]);

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

/** Reactive state collections from the durable stream (out-of-band observer). */
export function useCollections(): DurableACPCollections {
  return useDurableAcp().db.collections;
}

/** Raw DurableACPDB instance. */
export function useDb(): DurableACPDB {
  return useDurableAcp().db;
}

/** Resolved conductor endpoint URLs. */
export function useEndpoints(): ResolvedEndpoints {
  return useDurableAcp().endpoints;
}

// ============================================================================
// ACP Session Hook
// ============================================================================

export interface UseSessionOptions {
  onSessionUpdate?: (notification: SessionNotification) => void;
  onPermissionRequest?: (
    request: RequestPermissionRequest,
  ) => Promise<RequestPermissionResponse>;
  cwd?: string;
}

export interface SessionState {
  connection: ClientSideConnection | null;
  sessionId: string | null;
  isReady: boolean;
  error: Error | null;
  prompt: (text: string) => Promise<void>;
  cancel: () => Promise<void>;
  disconnect: () => void;
}

/**
 * Get the singleton ACP session created by the provider.
 *
 * The connection is managed in DurableAcpProvider — this hook
 * is a simple context consumer. Safe to call from any component
 * without creating duplicate WebSocket connections.
 */
export function useSession(_options?: UseSessionOptions): SessionState {
  return useDurableAcp().session;
}
