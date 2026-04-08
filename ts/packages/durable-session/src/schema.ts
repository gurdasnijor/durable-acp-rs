import { createStateSchema } from "@durable-streams/state";
import { z } from "zod";

export const durableACPState = createStateSchema({
  connections: {
    schema: z.object({
      logicalConnectionId: z.string(),
      state: z.enum(["created", "attached", "broken", "closed"]),
      latestSessionId: z.string().optional(),
      lastError: z.string().optional(),
      queuePaused: z.boolean().optional(),
      createdAt: z.number(),
      updatedAt: z.number(),
    }),
    type: "connection",
    primaryKey: "logicalConnectionId",
  },

  promptTurns: {
    schema: z.object({
      promptTurnId: z.string(),
      logicalConnectionId: z.string(),
      sessionId: z.string(),
      requestId: z.string(),
      text: z.string().optional(),
      state: z.enum([
        "queued",
        "active",
        "completed",
        "cancel_requested",
        "cancelled",
        "broken",
        "timed_out",
      ]),
      position: z.number().optional(),
      stopReason: z.string().optional(),
      startedAt: z.number(),
      completedAt: z.number().optional(),
    }),
    type: "prompt_turn",
    primaryKey: "promptTurnId",
  },

  pendingRequests: {
    schema: z.object({
      requestId: z.string(),
      logicalConnectionId: z.string(),
      sessionId: z.string().optional(),
      promptTurnId: z.string().optional(),
      method: z.string(),
      direction: z.enum(["client_to_agent", "agent_to_client"]),
      state: z.enum(["pending", "resolved", "orphaned"]),
      createdAt: z.number(),
      resolvedAt: z.number().optional(),
    }),
    type: "pending_request",
    primaryKey: "requestId",
  },

  permissions: {
    schema: z.object({
      requestId: z.string(),
      jsonrpcId: z.union([z.string(), z.number()]),
      logicalConnectionId: z.string(),
      sessionId: z.string(),
      promptTurnId: z.string(),
      title: z.string().optional(),
      toolCallId: z.string().optional(),
      options: z
        .array(
          z.object({
            optionId: z.string(),
            name: z.string(),
            kind: z.string(),
          }),
        )
        .optional(),
      state: z.enum(["pending", "resolved", "orphaned"]),
      outcome: z.string().optional(),
      createdAt: z.number(),
      resolvedAt: z.number().optional(),
    }),
    type: "permission",
    primaryKey: "requestId",
  },

  terminals: {
    schema: z.object({
      terminalId: z.string(),
      logicalConnectionId: z.string(),
      sessionId: z.string(),
      promptTurnId: z.string().optional(),
      state: z.enum(["open", "exited", "released", "broken"]),
      command: z.string().optional(),
      exitCode: z.number().optional(),
      signal: z.string().optional(),
      createdAt: z.number(),
      updatedAt: z.number(),
    }),
    type: "terminal",
    primaryKey: "terminalId",
  },

  runtimeInstances: {
    schema: z.object({
      instanceId: z.string(),
      runtimeName: z.string(),
      status: z.enum(["running", "paused", "stopped"]),
      createdAt: z.number(),
      updatedAt: z.number(),
    }),
    type: "runtime_instance",
    primaryKey: "instanceId",
  },

  chunks: {
    schema: z.object({
      chunkId: z.string(),
      promptTurnId: z.string(),
      logicalConnectionId: z.string(),
      type: z.enum(["text", "tool_call", "thinking", "tool_result", "error", "stop"]),
      content: z.string(),
      seq: z.number(),
      createdAt: z.number(),
    }),
    type: "chunk",
    primaryKey: "chunkId",
  },
});
