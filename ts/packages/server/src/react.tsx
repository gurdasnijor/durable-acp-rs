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
  children: React.ReactNode;
}

export function DurableAcpProvider({ basePath, children }: DurableAcpProviderProps) {
  const endpoints = useMemo(() => resolveEndpoints(basePath), [basePath]);

  const db = useMemo(
    () => createDurableACPDB({ stateStreamUrl: endpoints.streamUrl }),
    [endpoints.streamUrl],
  );

  useEffect(() => {
    db.preload();
    return () => db.close();
  }, [db]);

  const value = useMemo(() => ({ endpoints, db }), [endpoints, db]);

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
 * Connect to the conductor as a standard ACP client.
 *
 * Creates a ClientSideConnection over WebSocket, initializes the
 * protocol, and creates a session. Prompts, cancel, permissions go
 * through ACP. State observation comes from useCollections().
 */
export function useSession(options?: UseSessionOptions): SessionState {
  const { endpoints } = useDurableAcp();
  const [connection, setConnection] = useState<ClientSideConnection | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const streamRef = useRef<ReturnType<typeof fromWebSocket> | null>(null);
  const optionsRef = useRef(options);
  optionsRef.current = options;

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
  }, [endpoints.acpUrl]);

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

  return { connection, sessionId, isReady: !!connection && !!sessionId, error, prompt, cancel, disconnect };
}
