/**
 * DurableACPClient — enhanced ACP client.
 *
 * Subscribes to a durable state stream for reactive UX beyond standard ACP:
 * - Queue visibility (position, pause state, pending count)
 * - Durable history (reconnect and see everything that happened)
 * - Multi-session dashboards
 * - Real-time collection subscriptions
 *
 * POSTs commands to the conductor via HTTP.
 */

import { createOptimisticAction } from "@tanstack/db";
import type { Transaction, ChangeMessage, Collection } from "@tanstack/db";
import {
  createDurableACPDB,
  type DurableACPDB,
  type DurableACPCollections,
  type ConnectionRow,
  type PromptTurnRow,
  type PermissionRow,
  type PendingRequestRow,
  type TerminalRow,
  type RuntimeInstanceRow,
  type ChunkRow,
  createQueuedTurnsCollection,
  createActiveTurnsCollection,
  createPendingPermissionsCollection,
} from "@durable-acp/state";
import type { DurableACPClientOptions } from "./types";
import { WsTransport } from "./ws-transport";

// ============================================================================
// Client Collections Type
// ============================================================================

export interface DurableACPClientCollections extends DurableACPCollections {
  queuedTurns: Collection<PromptTurnRow>
  activeTurns: Collection<PromptTurnRow>
  pendingPermissions: Collection<PermissionRow>
}

// ============================================================================
// DurableACPClient
// ============================================================================

export class DurableACPClient {
  readonly connectionId: string

  private readonly _db: DurableACPDB
  private readonly _collections: DurableACPClientCollections
  private readonly _options: DurableACPClientOptions

  private _isConnected = false
  private _isDisposed = false
  private _error: Error | undefined
  private _wsTransport: WsTransport | null = null

  private readonly _abortController: AbortController

  private readonly _promptAction: (input: {
    promptTurnId: string
    logicalConnectionId: string
    text: string
    position: number
  }) => Transaction
  private readonly _resolvePermissionAction: (input: {
    requestId: string
    optionId: string
  }) => Transaction

  constructor(options: DurableACPClientOptions) {
    this._options = options
    this.connectionId = options.connectionId
    this._abortController = new AbortController()

    this._db =
      options._db ??
      createDurableACPDB({
        stateStreamUrl: options.stateStreamUrl,
        headers: options.headers,
        signal: this._abortController.signal,
      })

    this._collections = this.createCollections()
    this._promptAction = this.createPromptAction()
    this._resolvePermissionAction = this.createResolvePermissionAction()
    this.setupLifecycleCallbacks()
  }

  private setupLifecycleCallbacks(): void {
    const { onChunk, onTurnComplete, onPermission } = this._options

    if (onTurnComplete) {
      this._collections.promptTurns.subscribeChanges(
        (changes: ChangeMessage<PromptTurnRow>[]) => {
          for (const c of changes) {
            if (c.value?.state === "completed") onTurnComplete(c.value)
          }
        },
      )
    }

    if (onPermission) {
      this._collections.permissions.subscribeChanges(
        (changes: ChangeMessage<PermissionRow>[]) => {
          for (const c of changes) {
            if (c.type === "insert" && c.value?.state === "pending")
              onPermission(c.value)
          }
        },
      )
    }

    if (onChunk) {
      this._collections.chunks.subscribeChanges(
        (changes: ChangeMessage<ChunkRow>[]) => {
          for (const c of changes) {
            if (c.type === "insert") onChunk(c.value)
          }
        },
      )
    }
  }

  // ═══════════════════════════════════════════════════════════════════════
  // Collection Setup
  // ═══════════════════════════════════════════════════════════════════════

  private createCollections(): DurableACPClientCollections {
    const {
      connections,
      promptTurns,
      pendingRequests,
      permissions,
      terminals,
      runtimeInstances,
      chunks,
    } = this._db.collections

    const queuedTurns = createQueuedTurnsCollection({ promptTurns })
    const activeTurns = createActiveTurnsCollection({ promptTurns })
    const pendingPermissions = createPendingPermissionsCollection({
      permissions,
    })

    return {
      connections,
      promptTurns,
      pendingRequests,
      permissions,
      terminals,
      runtimeInstances,
      chunks,
      queuedTurns,
      activeTurns,
      pendingPermissions,
    }
  }

  // ═══════════════════════════════════════════════════════════════════════
  // Collections
  // ═══════════════════════════════════════════════════════════════════════

  get collections(): DurableACPClientCollections {
    return this._collections
  }

  get isLoading(): boolean {
    return this._collections.activeTurns.size > 0
  }

  get error(): Error | undefined {
    return this._error
  }

  get isDisposed(): boolean {
    return this._isDisposed
  }

  // ═══════════════════════════════════════════════════════════════════════
  // Typed Accessors
  // ═══════════════════════════════════════════════════════════════════════

  getConnection(id: string): ConnectionRow | undefined {
    return this._collections.connections.get(id)
  }
  getConnections(): ConnectionRow[] {
    return this._collections.connections.toArray
  }
  getPromptTurn(id: string): PromptTurnRow | undefined {
    return this._collections.promptTurns.get(id)
  }
  getPromptTurns(): PromptTurnRow[] {
    return this._collections.promptTurns.toArray
  }
  getPermission(id: string): PermissionRow | undefined {
    return this._collections.permissions.get(id)
  }
  getPermissions(): PermissionRow[] {
    return this._collections.permissions.toArray
  }
  getPendingRequests(): PendingRequestRow[] {
    return this._collections.pendingRequests.toArray
  }
  getTerminals(): TerminalRow[] {
    return this._collections.terminals.toArray
  }
  getRuntimeInstances(): RuntimeInstanceRow[] {
    return this._collections.runtimeInstances.toArray
  }

  // ═══════════════════════════════════════════════════════════════════════
  // Change Subscriptions
  // ═══════════════════════════════════════════════════════════════════════

  onPromptTurnChanges(
    cb: (changes: ChangeMessage<PromptTurnRow>[]) => void,
  ): { unsubscribe(): void } {
    return this._collections.promptTurns.subscribeChanges(cb)
  }
  onConnectionChanges(
    cb: (changes: ChangeMessage<ConnectionRow>[]) => void,
  ): { unsubscribe(): void } {
    return this._collections.connections.subscribeChanges(cb)
  }
  onPermissionChanges(
    cb: (changes: ChangeMessage<PermissionRow>[]) => void,
  ): { unsubscribe(): void } {
    return this._collections.permissions.subscribeChanges(cb)
  }
  onChunkChanges(
    cb: (changes: ChangeMessage<ChunkRow>[]) => void,
  ): { unsubscribe(): void } {
    return this._collections.chunks.subscribeChanges(cb)
  }

  // ═══════════════════════════════════════════════════════════════════════
  // Commands (POST to conductor)
  // ═══════════════════════════════════════════════════════════════════════

  async prompt(text: string): Promise<string> {
    if (!this._isConnected) {
      throw new Error("Client not connected. Call connect() first.")
    }

    const promptTurnId = crypto.randomUUID()
    const queuedForConnection = this._collections.queuedTurns.toArray.filter(
      (t: PromptTurnRow) => t.logicalConnectionId === this.connectionId,
    )
    const position = queuedForConnection.length

    await this.executeAction(this._promptAction, {
      promptTurnId,
      logicalConnectionId: this.connectionId,
      text,
      position,
    })

    return promptTurnId
  }

  async cancel(): Promise<void> {
    if (!this._isConnected) {
      throw new Error("Client not connected. Call connect() first.")
    }
    await this.postToServer(`/connections/${this.connectionId}/cancel`, {})
  }

  async pause(): Promise<void> {
    if (!this._isConnected) {
      throw new Error("Client not connected. Call connect() first.")
    }
    await this.postToServer(
      `/connections/${this.connectionId}/queue/pause`,
      {},
    )
  }

  async resume(): Promise<void> {
    if (!this._isConnected) {
      throw new Error("Client not connected. Call connect() first.")
    }
    await this.postToServer(
      `/connections/${this.connectionId}/queue/resume`,
      {},
    )
  }

  async reorder(turnIds: string[]): Promise<void> {
    if (!this._isConnected) {
      throw new Error("Client not connected. Call connect() first.")
    }
    await this.postToServer(`/connections/${this.connectionId}/queue`, {
      turnIds,
    })
  }

  async resolvePermission(
    requestId: string,
    optionId: string,
  ): Promise<void> {
    if (!this._isConnected) {
      throw new Error("Client not connected. Call connect() first.")
    }
    await this.executeAction(this._resolvePermissionAction, {
      requestId,
      optionId,
    })
  }

  // ═══════════════════════════════════════════════════════════════════════
  // Optimistic Actions
  // ═══════════════════════════════════════════════════════════════════════

  private async executeAction<T>(
    action: (input: T) => Transaction,
    input: T,
  ): Promise<void> {
    try {
      const transaction = action(input)
      await transaction.isPersisted.promise
    } catch (error) {
      this._error =
        error instanceof Error ? error : new Error(String(error))
      this._options.onError?.(this._error)
      throw error
    }
  }

  private createPromptAction() {
    return createOptimisticAction<{
      promptTurnId: string
      logicalConnectionId: string
      text: string
      position: number
    }>({
      onMutate: ({ promptTurnId, logicalConnectionId, text, position }) => {
        this._collections.promptTurns.insert({
          promptTurnId,
          logicalConnectionId,
          sessionId: "",
          requestId: promptTurnId,
          text,
          state: "queued",
          position,
          startedAt: 0,
        })
      },
      mutationFn: async ({ promptTurnId, logicalConnectionId, text }) => {
        await this.postToServer(
          `/connections/${logicalConnectionId}/prompt`,
          { text, promptTurnId },
        )
      },
    })
  }

  private createResolvePermissionAction() {
    return createOptimisticAction<{
      requestId: string
      optionId: string
    }>({
      onMutate: ({ requestId }) => {
        if (this._collections.permissions.has(requestId)) {
          this._collections.permissions.update(requestId, (draft) => {
            draft.state = "resolved"
            draft.resolvedAt = Date.now()
          })
        }
      },
      mutationFn: async ({ requestId, optionId }) => {
        await this.postToServer(`/permissions/${requestId}/resolve`, {
          optionId,
        })
      },
    })
  }

  // ═══════════════════════════════════════════════════════════════════════
  // Network
  // ═══════════════════════════════════════════════════════════════════════

  private async postToServer(
    path: string,
    body: Record<string, unknown>,
  ): Promise<void> {
    const response = await fetch(`${this._options.serverUrl}${path}`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        ...this._options.headers,
      },
      body: JSON.stringify(body),
    })

    if (!response.ok) {
      const errorText = await response.text()
      throw new Error(`Request failed: ${response.status} ${errorText}`)
    }
  }

  // ═══════════════════════════════════════════════════════════════════════
  // Lifecycle
  // ═══════════════════════════════════════════════════════════════════════

  async connect(): Promise<void> {
    if (this._isConnected) return

    try {
      await this._db.preload()

      // Derived collections require preload() to start their sync —
      // without this they never subscribe to source collection changes
      await Promise.all([
        this._collections.queuedTurns.preload(),
        this._collections.activeTurns.preload(),
        this._collections.pendingPermissions.preload(),
      ])

      // Optional WebSocket transport for real-time push events
      if (this._options.wsUrl) {
        this._wsTransport = new WsTransport({
          wsUrl: this._options.wsUrl,
          connectionId: this.connectionId,
          onChunk: this._options.onChunk,
          onTurnUpdate: (turn) => {
            if (turn.state === "completed") this._options.onTurnComplete?.(turn)
          },
          onPermission: (perm) => {
            if (perm.state === "pending") this._options.onPermission?.(perm)
          },
          onError: this._options.onError,
        })
        await this._wsTransport.connect()
      }

      this._isConnected = true
    } catch (error) {
      this._error =
        error instanceof Error ? error : new Error(String(error))
      this._options.onError?.(this._error)
      throw error
    }
  }

  disconnect(): void {
    this._wsTransport?.disconnect()
    this._wsTransport = null
    this._db.close()
    this._abortController.abort()
    this._isConnected = false
  }

  dispose(): void {
    if (this._isDisposed) return
    this._isDisposed = true
    this.disconnect()
  }
}

export function createDurableACPClient(
  options: DurableACPClientOptions,
): DurableACPClient {
  return new DurableACPClient(options)
}
