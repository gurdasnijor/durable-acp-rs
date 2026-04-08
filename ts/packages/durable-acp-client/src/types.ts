import type { DurableACPDB } from "@durable-acp/state";

/**
 * Configuration options for DurableACPClient.
 */
export interface DurableACPClientOptions {
  /** Logical connection identifier */
  connectionId: string
  /** Server URL for POSTing commands (e.g., http://localhost:4001/api/v1) */
  serverUrl: string
  /** URL of the state stream for subscribing to reactive data (SSE transport) */
  stateStreamUrl: string
  /** WebSocket URL for real-time push transport (e.g., ws://localhost:4500/ws). When set, creates a WS connection for event callbacks alongside StreamDB. */
  wsUrl?: string
  /** Additional headers for requests */
  headers?: Record<string, string>

  // Lifecycle callbacks
  /** Called when a chunk is written (text, tool_call, thinking, etc.) */
  onChunk?: (chunk: import("@durable-acp/state").ChunkRow) => void
  /** Called when a prompt turn completes */
  onTurnComplete?: (turn: import("@durable-acp/state").PromptTurnRow) => void
  /** Called when a permission request arrives */
  onPermission?: (perm: import("@durable-acp/state").PermissionRow) => void
  /** Called on error */
  onError?: (error: Error) => void

  /**
   * Pre-created DurableACPDB for testing.
   * @internal
   */
  _db?: DurableACPDB
}
