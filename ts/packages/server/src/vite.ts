/**
 * Vite plugin — proxies to a running durable-acp-rs conductor.
 *
 * Usage:
 *   import { durableAcpProxy } from "@durable-acp/server/vite";
 *   export default defineConfig({
 *     plugins: [durableAcpProxy()],
 *   });
 *
 * Then in React: fetch("/streams/..."), new WebSocket("/acp"), fetch("/api/...")
 * All proxied to the conductor. No ports in client code.
 */

import type { Plugin } from "vite";

export interface DurableAcpProxyOptions {
  /** Base port of the conductor. Streams on :port, API on :port+1. Default: 4437. */
  port?: number;
  /** Host. Default: "127.0.0.1". */
  host?: string;
}

export function durableAcpProxy(options?: DurableAcpProxyOptions): Plugin {
  const host = options?.host ?? "127.0.0.1";
  const streamsPort = options?.port ?? 4437;
  const apiPort = streamsPort + 1;

  return {
    name: "durable-acp-proxy",

    config() {
      return {
        server: {
          proxy: {
            // ACP WebSocket — prompt, cancel, permissions
            "/acp": {
              target: `ws://${host}:${apiPort}`,
              ws: true,
            },
            // Durable streams — SSE state observation
            "/streams": {
              target: `http://${host}:${streamsPort}`,
            },
            // REST API — queue management, filesystem, registry
            "/api": {
              target: `http://${host}:${apiPort}`,
            },
          },
        },
      };
    },
  };
}
