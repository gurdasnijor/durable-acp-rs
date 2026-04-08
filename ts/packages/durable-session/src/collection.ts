/**
 * Durable ACP stream-db factory.
 *
 * Creates a stream-backed database using @durable-streams/state for syncing
 * state from Durable Streams. The resulting DurableACPDB provides typed
 * collections (connections, promptTurns, permissions, chunks, etc.) that are
 * automatically populated from STATE-PROTOCOL events on the stream.
 */

import { createStreamDB, type StreamDB, type StreamDBMethods } from '@durable-streams/state'
import type { Collection } from '@tanstack/db'
import { durableACPState } from './schema'
import type {
  ConnectionRow,
  PromptTurnRow,
  PendingRequestRow,
  PermissionRow,
  TerminalRow,
  RuntimeInstanceRow,
  ChunkRow,
} from './types'

// ============================================================================
// DurableACPDB Types
// ============================================================================

/**
 * Collections map with correct row types.
 */
export interface DurableACPCollections {
  connections: Collection<ConnectionRow>
  promptTurns: Collection<PromptTurnRow>
  pendingRequests: Collection<PendingRequestRow>
  permissions: Collection<PermissionRow>
  terminals: Collection<TerminalRow>
  runtimeInstances: Collection<RuntimeInstanceRow>
  chunks: Collection<ChunkRow>
}

/**
 * Type alias for a Durable ACP stream-db instance.
 *
 * Provides typed access to all collections plus stream-db methods:
 * - `db.preload()` - Wait for initial sync
 * - `db.close()` - Cleanup resources
 * - `db.utils.awaitTxId(txid)` - Wait for specific write to sync
 */
export type DurableACPDB = {
  collections: DurableACPCollections
} & StreamDBMethods

/**
 * Internal type for the raw stream-db instance.
 * @internal
 */
type RawDurableACPDB = StreamDB<typeof durableACPState>

// ============================================================================
// Configuration
// ============================================================================

/**
 * Configuration for creating a Durable ACP stream-db.
 */
export interface DurableACPDBConfig {
  /** URL of the state stream */
  stateStreamUrl: string
  /** Additional headers for stream requests */
  headers?: Record<string, string>
  /** AbortSignal to cancel the stream sync */
  signal?: AbortSignal
}

// ============================================================================
// DurableACPDB Factory
// ============================================================================

/**
 * Create a stream-db instance for Durable ACP.
 *
 * This function is synchronous — it creates the stream handle and collections
 * but does not start the stream connection. Call `db.preload()` to connect
 * and wait for the initial sync to complete.
 *
 * @example
 * ```typescript
 * const db = createDurableACPDB({
 *   stateStreamUrl: 'http://localhost:3000/durable-acp-state',
 * })
 *
 * await db.preload()
 *
 * for (const turn of db.collections.promptTurns.values()) {
 *   console.log(turn.promptTurnId, turn.state)
 * }
 *
 * db.close()
 * ```
 */
export function createDurableACPDB(config: DurableACPDBConfig): DurableACPDB {
  const { stateStreamUrl, headers, signal } = config

  const rawDb: RawDurableACPDB = createStreamDB({
    streamOptions: {
      url: stateStreamUrl,
      headers,
      signal,
    },
    state: durableACPState,
  })

  // Cast to our DurableACPDB type which has correctly typed collections
  return rawDb as unknown as DurableACPDB
}
