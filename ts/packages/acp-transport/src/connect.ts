/**
 * High-level connect helpers for ACP clients.
 *
 * Abstracts transport setup + protocol initialization into a single call.
 */

import {
  ClientSideConnection,
  type Client,
  type Agent,
  type Stream,
  type SessionNotification,
  type RequestPermissionRequest,
  type RequestPermissionResponse,
} from "@agentclientprotocol/sdk";
import { fromWebSocket, type WebSocketStreamOptions } from "./ws";

// ============================================================================
// Types
// ============================================================================

/**
 * Handlers for ACP events. All optional — unhandled events are silently ignored
 * (permissions auto-approve first option by default).
 */
export interface AcpClientHandlers {
  /** Called on each session update (text chunks, tool calls, thinking, etc.) */
  onSessionUpdate?: (notification: SessionNotification) => void;
  /** Called when the agent requests permission. Return the user's choice. */
  onPermissionRequest?: (
    request: RequestPermissionRequest,
  ) => Promise<RequestPermissionResponse>;
  /** Called on connection error. */
  onError?: (error: Error) => void;
}

/**
 * A connected ACP session — ready to send prompts.
 */
export interface AcpConnection {
  /** The underlying SDK connection (for advanced use). */
  connection: ClientSideConnection;
  /** The session ID from the conductor. */
  sessionId: string;
  /** Send a text prompt and wait for the response. */
  prompt(text: string): ReturnType<ClientSideConnection["prompt"]>;
  /** Cancel the current prompt turn. */
  cancel(): Promise<void>;
  /** Disconnect from the conductor. */
  disconnect(): void;
}

// ============================================================================
// Connect via WebSocket
// ============================================================================

/**
 * Connect to a conductor via WebSocket, initialize, and create a session.
 *
 * Returns an AcpConnection ready to send prompts.
 *
 * ```typescript
 * import { connectWs } from "@durable-acp/transport";
 *
 * const conn = await connectWs("ws://host:4438/acp", {
 *   onSessionUpdate: (n) => console.log("update:", n),
 * });
 *
 * const response = await conn.prompt("hello");
 * conn.disconnect();
 * ```
 */
export async function connectWs(
  url: string,
  handlers?: AcpClientHandlers & {
    /** Working directory for the session. */
    cwd?: string;
    /** Client name reported to the conductor. */
    clientName?: string;
    /** WebSocket transport options. */
    wsOptions?: WebSocketStreamOptions;
  },
): Promise<AcpConnection> {
  const stream = fromWebSocket(url, handlers?.wsOptions);
  return connectStream(stream, handlers);
}

// ============================================================================
// Connect via any Stream
// ============================================================================

/**
 * Connect to a conductor via an arbitrary ACP Stream.
 *
 * Use this when you have a custom transport (e.g., in-memory for testing,
 * or a pre-established WebSocket).
 */
export async function connectStream(
  stream: Stream & { close(): void },
  handlers?: AcpClientHandlers & {
    cwd?: string;
    clientName?: string;
  },
): Promise<AcpConnection> {
  const conn = new ClientSideConnection(
    (_agent: Agent): Client => ({
      requestPermission: async (params) => {
        if (handlers?.onPermissionRequest) {
          return handlers.onPermissionRequest(params);
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
        handlers?.onSessionUpdate?.(params);
      },
    }),
    stream,
  );

  // Initialize
  await conn.initialize({
    protocolVersion: "2025-03-26",
    clientInfo: {
      name: handlers?.clientName ?? "acp-client",
      version: "0.1.0",
    },
    capabilities: {},
  } as any);

  // Create session
  const cwd = handlers?.cwd ?? (typeof process !== "undefined" ? process.cwd?.() : "/");
  const sessionResponse = await conn.newSession({ cwd } as any);
  const sessionId = sessionResponse.sessionId;

  return {
    connection: conn,
    sessionId,
    async prompt(text: string) {
      return conn.prompt({
        sessionId,
        messages: [{ role: "user", content: { type: "text", text } }],
      } as any);
    },
    async cancel() {
      await conn.cancel({ sessionId } as any);
    },
    disconnect() {
      stream.close();
    },
  };
}
