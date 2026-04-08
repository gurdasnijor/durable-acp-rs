// ============================================================================
// Row types — manual interfaces matching schema.ts definitions.
//
// Defined manually instead of using z.infer<> because
// createStateSchema() from @durable-streams/state doesn't preserve
// schema types in its return type. Manual interfaces satisfy the
// `extends object` constraint on Collection<T>.
// ============================================================================

export interface ConnectionRow {
  logicalConnectionId: string
  state: 'created' | 'attached' | 'broken' | 'closed'
  latestSessionId?: string
  lastError?: string
  queuePaused?: boolean
  createdAt: number
  updatedAt: number
}

export interface PromptTurnRow {
  promptTurnId: string
  logicalConnectionId: string
  sessionId: string
  requestId: string
  text?: string
  state:
    | 'queued'
    | 'active'
    | 'completed'
    | 'cancel_requested'
    | 'cancelled'
    | 'broken'
    | 'timed_out'
  position?: number
  stopReason?: string
  startedAt: number
  completedAt?: number
}

export interface PendingRequestRow {
  requestId: string
  logicalConnectionId: string
  sessionId?: string
  promptTurnId?: string
  method: string
  direction: 'client_to_agent' | 'agent_to_client'
  state: 'pending' | 'resolved' | 'orphaned'
  createdAt: number
  resolvedAt?: number
}

export interface PermissionRow {
  requestId: string
  jsonrpcId: string | number
  logicalConnectionId: string
  sessionId: string
  promptTurnId: string
  title?: string
  toolCallId?: string
  options?: Array<{
    optionId: string
    name: string
    kind: string
  }>
  state: 'pending' | 'resolved' | 'orphaned'
  outcome?: string
  createdAt: number
  resolvedAt?: number
}

export interface TerminalRow {
  terminalId: string
  logicalConnectionId: string
  sessionId: string
  promptTurnId?: string
  state: 'open' | 'exited' | 'released' | 'broken'
  command?: string
  exitCode?: number
  signal?: string
  createdAt: number
  updatedAt: number
}

export interface RuntimeInstanceRow {
  instanceId: string
  runtimeName: string
  status: 'running' | 'paused' | 'stopped'
  createdAt: number
  updatedAt: number
}

export interface ChunkRow {
  chunkId: string
  promptTurnId: string
  logicalConnectionId: string
  type: 'text' | 'tool_call' | 'thinking' | 'tool_result' | 'error' | 'stop'
  content: string
  seq: number
  createdAt: number
}

export type ConnectionStatus = 'disconnected' | 'connecting' | 'connected' | 'error'
