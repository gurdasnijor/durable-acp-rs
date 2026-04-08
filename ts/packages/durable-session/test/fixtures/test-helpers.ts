/**
 * Test utilities for @durable-acp/state.
 *
 * Provides mock DurableACPDB with controlled collections for testing
 * without network. Inject mock DB via constructor options (_db).
 */

import { createCollection } from '@tanstack/db'
import type { Collection, SyncConfig, ChangeMessage } from '@tanstack/db'
import type {
  ConnectionRow,
  PromptTurnRow,
  PendingRequestRow,
  PermissionRow,
  TerminalRow,
  RuntimeInstanceRow,
  ChunkRow,
} from '../../src/types'
import type { DurableACPDB } from '../../src/collection'

// ============================================================================
// Generic Mock Collection
// ============================================================================

export interface MockCollectionController<T extends object> {
  emit: (rows: T[]) => void
  markReady: () => void
  utils: {
    begin: () => void
    write: (change: ChangeMessage<T>) => void
    commit: () => void
    markReady: () => void
  }
}

function createMockCollection<T extends object>(
  id: string,
  getKey: (row: T) => string
): {
  collection: Collection<T>
  controller: MockCollectionController<T>
} {
  let begin!: () => void
  let write!: (change: ChangeMessage<T>) => void
  let commit!: () => void
  let markReadyFn!: () => void

  const sync: SyncConfig<T>['sync'] = (params) => {
    begin = params.begin
    write = params.write
    commit = params.commit
    markReadyFn = params.markReady
  }

  const collection = createCollection<T>({
    id,
    getKey,
    sync: { sync },
  })

  const controller: MockCollectionController<T> = {
    emit: (rows: T[]) => {
      begin()
      for (const row of rows) {
        write({ type: 'insert', value: row })
      }
      commit()
    },
    markReady: () => {
      markReadyFn()
    },
    utils: {
      begin: () => begin(),
      write: (change) => write(change),
      commit: () => commit(),
      markReady: () => markReadyFn(),
    },
  }

  return { collection, controller }
}

// ============================================================================
// Mock DurableACPDB Factory
// ============================================================================

export interface MockDurableACPDBControllers {
  connections: MockCollectionController<ConnectionRow>
  promptTurns: MockCollectionController<PromptTurnRow>
  pendingRequests: MockCollectionController<PendingRequestRow>
  permissions: MockCollectionController<PermissionRow>
  terminals: MockCollectionController<TerminalRow>
  runtimeInstances: MockCollectionController<RuntimeInstanceRow>
  chunks: MockCollectionController<ChunkRow>
}

/**
 * Creates a mock DurableACPDB for testing.
 *
 * @example
 * ```typescript
 * const { db, controllers } = createMockDurableACPDB()
 * // use db for testing, controllers to emit data
 * controllers.promptTurns.emit([{ ... }])
 * ```
 */
export function createMockDurableACPDB(): {
  db: DurableACPDB
  controllers: MockDurableACPDBControllers
} {
  const { collection: connections, controller: connectionsCtrl } =
    createMockCollection<ConnectionRow>(
      'test-connections',
      (row) => row.logicalConnectionId
    )

  const { collection: promptTurns, controller: promptTurnsCtrl } =
    createMockCollection<PromptTurnRow>(
      'test-promptTurns',
      (row) => row.promptTurnId
    )

  const { collection: pendingRequests, controller: pendingRequestsCtrl } =
    createMockCollection<PendingRequestRow>(
      'test-pendingRequests',
      (row) => row.requestId
    )

  const { collection: permissions, controller: permissionsCtrl } =
    createMockCollection<PermissionRow>(
      'test-permissions',
      (row) => row.requestId
    )

  const { collection: terminals, controller: terminalsCtrl } =
    createMockCollection<TerminalRow>(
      'test-terminals',
      (row) => row.terminalId
    )

  const { collection: runtimeInstances, controller: runtimeInstancesCtrl } =
    createMockCollection<RuntimeInstanceRow>(
      'test-runtimeInstances',
      (row) => row.instanceId
    )

  const { collection: chunks, controller: chunksCtrl } =
    createMockCollection<ChunkRow>(
      'test-chunks',
      (row) => row.chunkId
    )

  const allControllers: MockDurableACPDBControllers = {
    connections: connectionsCtrl,
    promptTurns: promptTurnsCtrl,
    pendingRequests: pendingRequestsCtrl,
    permissions: permissionsCtrl,
    terminals: terminalsCtrl,
    runtimeInstances: runtimeInstancesCtrl,
    chunks: chunksCtrl,
  }

  const db = {
    collections: {
      connections,
      promptTurns,
      pendingRequests,
      permissions,
      terminals,
      runtimeInstances,
      chunks,
    },
    stream: null,
    preload: async () => {
      connections.preload()
      promptTurns.preload()
      pendingRequests.preload()
      permissions.preload()
      terminals.preload()
      runtimeInstances.preload()
      chunks.preload()
      connectionsCtrl.markReady()
      promptTurnsCtrl.markReady()
      pendingRequestsCtrl.markReady()
      permissionsCtrl.markReady()
      terminalsCtrl.markReady()
      runtimeInstancesCtrl.markReady()
      chunksCtrl.markReady()
    },
    close: () => {},
    utils: {
      awaitTxId: async () => {},
    },
  } as unknown as DurableACPDB

  return { db, controllers: allControllers }
}

// ============================================================================
// Utilities
// ============================================================================

/**
 * Flush microtasks and timers to allow async operations to complete.
 * Wait 40ms to give live query pipelines time to propagate changes.
 */
export async function flushPromises(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 40))
}

/**
 * Subscribe to changes and collect them into an array for assertions.
 */
export function collectChanges<T extends object>(
  collection: Collection<T>
): { changes: Array<{ type: string; key: string | number }>; unsubscribe: () => void } {
  const changes: Array<{ type: string; key: string | number }> = []
  const subscription = collection.subscribeChanges((changeSet) => {
    for (const change of changeSet) {
      changes.push({
        type: change.type,
        key: collection.getKeyFromItem(change.value),
      })
    }
  })
  return { changes, unsubscribe: () => subscription.unsubscribe() }
}
