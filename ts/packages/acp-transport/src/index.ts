// Transport adapters
export { fromWebSocket, fromExistingWebSocket, type WebSocketStreamOptions } from "./ws";

// Connect helpers
export { connectWs, connectStream, type AcpClientHandlers, type AcpConnection } from "./connect";

// Re-export Stream type for convenience
export type { Stream } from "@agentclientprotocol/sdk";
