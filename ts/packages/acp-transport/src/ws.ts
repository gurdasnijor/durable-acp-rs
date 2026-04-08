/**
 * WebSocket transport for ACP.
 *
 * Adapts a WebSocket connection into an ACP Stream that can be passed
 * to ClientSideConnection or AgentSideConnection.
 *
 * Each WebSocket text message = one JSON-RPC message.
 */

import type { Stream } from "@agentclientprotocol/sdk";

export interface WebSocketStreamOptions {
  /** Called when the WebSocket connection opens. */
  onOpen?: () => void;
  /** Called when the WebSocket connection closes. */
  onClose?: (code: number, reason: string) => void;
  /** Called on WebSocket error. */
  onError?: (error: Event) => void;
}

/**
 * Create an ACP Stream from a WebSocket URL.
 *
 * ```typescript
 * import { ClientSideConnection } from "@agentclientprotocol/sdk";
 * import { fromWebSocket } from "@durable-acp/transport";
 *
 * const stream = fromWebSocket("ws://host:4438/acp");
 * const conn = new ClientSideConnection(toClient, stream);
 * ```
 */
export function fromWebSocket(
  url: string,
  options?: WebSocketStreamOptions,
): Stream & { close(): void } {
  const ws = new WebSocket(url);

  // Track open state for synchronous write checks
  let opened = false;
  const openPromise = new Promise<void>((resolve, reject) => {
    ws.addEventListener("open", () => {
      opened = true;
      options?.onOpen?.();
      resolve();
    }, { once: true });
    ws.addEventListener("error", (e) => {
      if (!opened) reject(new Error("WebSocket failed to connect"));
    }, { once: true });
  });

  const readable = new ReadableStream({
    start(controller) {
      ws.addEventListener("message", (event) => {
        try {
          controller.enqueue(JSON.parse(String(event.data)));
        } catch {
          // skip malformed messages
        }
      });
      ws.addEventListener("close", (event) => {
        options?.onClose?.(event.code, event.reason);
        try { controller.close(); } catch { /* already closed */ }
      });
      ws.addEventListener("error", (e) => {
        options?.onError?.(e);
        try { controller.error(new Error("WebSocket error")); } catch { /* already errored */ }
      });
    },
  });

  const writable = new WritableStream({
    async write(msg) {
      if (!opened) await openPromise;
      ws.send(JSON.stringify(msg));
    },
    close() {
      if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) {
        ws.close();
      }
    },
  });

  return {
    readable,
    writable,
    close() {
      if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) {
        ws.close();
      }
    },
  };
}

/**
 * Create an ACP Stream from an existing WebSocket instance.
 *
 * Useful when you already have a WebSocket (e.g., from a framework's
 * upgrade handler) and want to wrap it as an ACP stream.
 */
export function fromExistingWebSocket(
  ws: WebSocket,
  options?: WebSocketStreamOptions,
): Stream & { close(): void } {
  const readable = new ReadableStream({
    start(controller) {
      ws.addEventListener("message", (event) => {
        try {
          controller.enqueue(JSON.parse(String(event.data)));
        } catch { /* skip malformed */ }
      });
      ws.addEventListener("close", (event) => {
        options?.onClose?.(event.code, event.reason);
        try { controller.close(); } catch { /* already closed */ }
      });
      ws.addEventListener("error", (e) => {
        options?.onError?.(e);
        try { controller.error(new Error("WebSocket error")); } catch { /* already errored */ }
      });
    },
  });

  const writable = new WritableStream({
    async write(msg) {
      if (ws.readyState === WebSocket.CONNECTING) {
        await new Promise<void>((resolve) =>
          ws.addEventListener("open", () => resolve(), { once: true })
        );
      }
      ws.send(JSON.stringify(msg));
    },
    close() {
      if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) {
        ws.close();
      }
    },
  });

  return {
    readable,
    writable,
    close() {
      if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) {
        ws.close();
      }
    },
  };
}
