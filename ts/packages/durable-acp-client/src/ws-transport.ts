/**
 * WebSocket transport for DurableACPClient.
 *
 * Subscribes to the server's /ws endpoint for real-time push events.
 * Provides the same reactive interface as the SSE-based StreamDB transport,
 * but over a single WebSocket connection. Ideal for browser UIs.
 */

import type {
  PromptTurnRow,
  ChunkRow,
  PermissionRow,
  ConnectionRow,
} from "@durable-acp/state";

export interface WsTransportOptions {
  /** WebSocket URL (e.g., ws://localhost:4500/ws) */
  wsUrl: string;
  /** Logical connection ID to subscribe to */
  connectionId: string;
  /** Called when a chunk is inserted */
  onChunk?: (chunk: ChunkRow) => void;
  /** Called when a prompt turn changes state */
  onTurnUpdate?: (turn: PromptTurnRow) => void;
  /** Called when a permission request arrives */
  onPermission?: (perm: PermissionRow) => void;
  /** Called when a connection state changes */
  onConnectionUpdate?: (conn: ConnectionRow) => void;
  /** Called on error */
  onError?: (error: Error) => void;
  /** Called when connected and subscriptions are active */
  onReady?: () => void;
}

interface WsEvent {
  type: string;
  channel?: string;
  wsId?: string;
  history?: Array<{ type: string; value: unknown }>;
  change?: { type: string; value: unknown };
  message?: string;
}

export class WsTransport {
  private ws: WebSocket | null = null;
  private readonly opts: WsTransportOptions;
  private pendingSubscriptions = 0;

  constructor(opts: WsTransportOptions) {
    this.opts = opts;
  }

  async connect(): Promise<void> {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(this.opts.wsUrl);
      this.ws = ws;

      ws.addEventListener("error", (e) => {
        const err = new Error("WebSocket error");
        this.opts.onError?.(err);
        reject(err);
      });

      ws.addEventListener("open", () => {
        // Subscribe to channels for this connection
        const channels = [
          `session:${this.opts.connectionId}`,
          `session:${this.opts.connectionId}:chunks`,
          `session:${this.opts.connectionId}:permissions`,
        ];
        this.pendingSubscriptions = channels.length;
        for (const ch of channels) {
          ws.send(JSON.stringify({ type: "subscribe", channel: ch }));
        }
      });

      ws.addEventListener("message", (event) => {
        const msg = JSON.parse(String(event.data)) as WsEvent;
        this.handleMessage(msg, resolve);
      });

      ws.addEventListener("close", () => {
        this.ws = null;
      });
    });
  }

  private handleMessage(msg: WsEvent, onReady?: (value: void) => void): void {
    switch (msg.type) {
      case "connected":
        // Wait for subscriptions
        break;

      case "subscribed": {
        // Replay history
        if (msg.history) {
          for (const entry of msg.history) {
            this.dispatchChange(msg.channel!, entry.type, entry.value);
          }
        }
        this.pendingSubscriptions--;
        if (this.pendingSubscriptions === 0) {
          this.opts.onReady?.();
          onReady?.();
        }
        break;
      }

      case "event":
        if (msg.change) {
          this.dispatchChange(msg.channel!, msg.change.type, msg.change.value);
        }
        break;

      case "error":
        this.opts.onError?.(new Error(msg.message ?? "Unknown WS error"));
        break;

      case "pong":
        break;
    }
  }

  private dispatchChange(channel: string, changeType: string, value: unknown): void {
    const connId = this.opts.connectionId;

    if (channel === `session:${connId}`) {
      this.opts.onTurnUpdate?.(value as PromptTurnRow);
    } else if (channel === `session:${connId}:chunks` && changeType === "insert") {
      this.opts.onChunk?.(value as ChunkRow);
    } else if (channel === `session:${connId}:permissions` && changeType === "insert") {
      this.opts.onPermission?.(value as PermissionRow);
    }
  }

  disconnect(): void {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.close();
    }
    this.ws = null;
  }

  ping(): void {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify({ type: "ping" }));
    }
  }
}
