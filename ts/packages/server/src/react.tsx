/**
 * React bindings for @durable-acp/server.
 *
 * Provides durable stream observation only (StreamDB + collections).
 * ACP connection (control) is handled by the consumer (e.g., use-acp).
 *
 * Usage:
 *   import { DurableStreamProvider, useCollections } from "@durable-acp/server/react";
 *
 *   <DurableStreamProvider>
 *   <DurableStreamProvider basePath="http://host:4437">
 */

import { createContext, useContext, useEffect, useMemo } from "react";
import { createDurableACPDB, type DurableACPDB, type DurableACPCollections } from "durable-session";

// ============================================================================
// URL resolution
// ============================================================================

export interface ResolvedEndpoints {
  acpUrl: string;
  streamUrl: string;
  apiUrl: string;
}

export function resolveEndpoints(basePath?: string): ResolvedEndpoints {
  if (basePath) {
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

  if (typeof window !== "undefined") {
    const wsProtocol = location.protocol === "https:" ? "wss:" : "ws:";
    return {
      acpUrl: `${wsProtocol}//${location.host}/acp`,
      streamUrl: `${location.origin}/streams/durable-acp-state`,
      apiUrl: location.origin,
    };
  }

  return {
    acpUrl: "ws://127.0.0.1:4438/acp",
    streamUrl: "http://127.0.0.1:4437/streams/durable-acp-state",
    apiUrl: "http://127.0.0.1:4438",
  };
}

// ============================================================================
// Context
// ============================================================================

interface DurableStreamContextValue {
  endpoints: ResolvedEndpoints;
  db: DurableACPDB;
}

const DurableStreamContext = createContext<DurableStreamContextValue | null>(null);

// ============================================================================
// Provider — observation only (durable stream)
// ============================================================================

export interface DurableStreamProviderProps {
  basePath?: string;
  children: React.ReactNode;
}

export function DurableStreamProvider({ basePath, children }: DurableStreamProviderProps) {
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
    <DurableStreamContext.Provider value={value}>
      {children}
    </DurableStreamContext.Provider>
  );
}

// ============================================================================
// Hooks
// ============================================================================

function useDurableStream(): DurableStreamContextValue {
  const ctx = useContext(DurableStreamContext);
  if (!ctx) throw new Error("useDurableStream must be used within <DurableStreamProvider>");
  return ctx;
}

/** Reactive state collections from the durable stream. */
export function useCollections(): DurableACPCollections {
  return useDurableStream().db.collections;
}

/** Raw DurableACPDB instance. */
export function useDb(): DurableACPDB {
  return useDurableStream().db;
}

/** Resolved conductor endpoint URLs. */
export function useEndpoints(): ResolvedEndpoints {
  return useDurableStream().endpoints;
}

// Re-export types
export type { DurableACPCollections, DurableACPDB } from "durable-session";

// Backwards compat aliases
export { DurableStreamProvider as DurableAcpProvider };
export type { DurableStreamProviderProps as DurableAcpProviderProps };
