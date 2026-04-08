/**
 * Tests for DurableACPDB collections and derived collection factories.
 *
 * Uses mock DB injection for testing without network.
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest'
import { createMockDurableACPDB, flushPromises, type MockDurableACPDBControllers } from './fixtures/test-helpers'
import {
  createQueuedTurnsCollection,
  createActiveTurnsCollection,
  createPendingPermissionsCollection,
} from '../src/collections/index'
import type { DurableACPDB } from '../src/collection'
import type { PromptTurnRow, PermissionRow, ChunkRow } from '../src/types'

describe('DurableACPDB collections', () => {
  let db: DurableACPDB
  let controllers: MockDurableACPDBControllers

  beforeEach(async () => {
    const mock = createMockDurableACPDB()
    db = mock.db
    controllers = mock.controllers
    await db.preload()
  })

  afterEach(() => {
    db.close()
  })

  it('creates DB with all 7 collections', () => {
    expect(db.collections.connections).toBeDefined()
    expect(db.collections.promptTurns).toBeDefined()
    expect(db.collections.pendingRequests).toBeDefined()
    expect(db.collections.permissions).toBeDefined()
    expect(db.collections.terminals).toBeDefined()
    expect(db.collections.runtimeInstances).toBeDefined()
    expect(db.collections.chunks).toBeDefined()
  })

  it('preload resolves and collections are accessible', () => {
    expect(typeof db.collections.connections.size).toBe('number')
    expect(typeof db.collections.promptTurns.size).toBe('number')
  })

  it('collections start empty', () => {
    expect(db.collections.connections.size).toBe(0)
    expect(db.collections.promptTurns.size).toBe(0)
    expect(db.collections.chunks.size).toBe(0)
  })

  it('close cleans up without error', () => {
    expect(() => db.close()).not.toThrow()
  })
})

describe('derived collections', () => {
  let db: DurableACPDB
  let controllers: MockDurableACPDBControllers

  beforeEach(async () => {
    const mock = createMockDurableACPDB()
    db = mock.db
    controllers = mock.controllers
    await db.preload()
  })

  afterEach(() => {
    db.close()
  })

  it('queuedTurns filters state=queued', async () => {
    controllers.promptTurns.emit([
      makeTurn('t1', 'queued', 0),
      makeTurn('t2', 'active'),
      makeTurn('t3', 'queued', 1),
      makeTurn('t4', 'completed'),
    ])

    const queuedTurns = createQueuedTurnsCollection({
      promptTurns: db.collections.promptTurns,
    })
    queuedTurns.preload()
    await flushPromises()

    const items = queuedTurns.toArray
    expect(items.length).toBe(2)
    expect(items.map((t: PromptTurnRow) => t.promptTurnId)).toEqual(['t1', 't3'])
  })

  it('queuedTurns orders by position ascending', async () => {
    controllers.promptTurns.emit([
      makeTurn('t1', 'queued', 2),
      makeTurn('t2', 'queued', 0),
      makeTurn('t3', 'queued', 1),
    ])

    const queuedTurns = createQueuedTurnsCollection({
      promptTurns: db.collections.promptTurns,
    })
    queuedTurns.preload()
    await flushPromises()

    const items = queuedTurns.toArray
    expect(items.map((t: PromptTurnRow) => t.promptTurnId)).toEqual(['t2', 't3', 't1'])
  })

  it('activeTurns filters state=active', async () => {
    controllers.promptTurns.emit([
      makeTurn('t1', 'queued', 0),
      makeTurn('t2', 'active'),
      makeTurn('t3', 'completed'),
    ])

    const activeTurns = createActiveTurnsCollection({
      promptTurns: db.collections.promptTurns,
    })
    activeTurns.preload()
    await flushPromises()

    const items = activeTurns.toArray
    expect(items.length).toBe(1)
    expect(items[0].promptTurnId).toBe('t2')
  })

  it('pendingPermissions filters state=pending', async () => {
    controllers.permissions.emit([
      makePermission('p1', 'pending'),
      makePermission('p2', 'resolved'),
      makePermission('p3', 'pending'),
    ])

    const pendingPerms = createPendingPermissionsCollection({
      permissions: db.collections.permissions,
    })
    pendingPerms.preload()
    await flushPromises()

    const items = pendingPerms.toArray
    expect(items.length).toBe(2)
    expect(items.map((p: PermissionRow) => p.requestId)).toContain('p1')
    expect(items.map((p: PermissionRow) => p.requestId)).toContain('p3')
  })

  it('derived collections update reactively on source changes', async () => {
    const queuedTurns = createQueuedTurnsCollection({
      promptTurns: db.collections.promptTurns,
    })
    queuedTurns.preload()

    // Start with one queued turn
    controllers.promptTurns.emit([makeTurn('t1', 'queued', 0)])
    await flushPromises()
    expect(queuedTurns.size).toBe(1)

    // Add another queued turn
    controllers.promptTurns.utils.begin()
    controllers.promptTurns.utils.write({
      type: 'insert',
      value: makeTurn('t2', 'queued', 1),
    })
    controllers.promptTurns.utils.commit()
    await flushPromises()

    expect(queuedTurns.size).toBe(2)

    // Transition t1 to active — should drop from queued
    controllers.promptTurns.utils.begin()
    controllers.promptTurns.utils.write({
      type: 'update',
      value: makeTurn('t1', 'active'),
    })
    controllers.promptTurns.utils.commit()
    await flushPromises()

    expect(queuedTurns.size).toBe(1)
    expect(queuedTurns.toArray[0].promptTurnId).toBe('t2')
  })
})

describe('subscribeChanges', () => {
  let db: DurableACPDB
  let controllers: MockDurableACPDBControllers

  beforeEach(async () => {
    const mock = createMockDurableACPDB()
    db = mock.db
    controllers = mock.controllers
    await db.preload()
  })

  afterEach(() => {
    db.close()
  })

  it('fires callback on collection insert', async () => {
    const inserts: ChunkRow[] = []
    db.collections.chunks.subscribeChanges((changes) => {
      for (const c of changes) {
        if (c.type === 'insert') inserts.push(c.value)
      }
    })

    controllers.chunks.emit([
      makeChunk('ch1', 't1', 'text', 'Hello', 0),
    ])
    await flushPromises()

    expect(inserts.length).toBe(1)
    expect(inserts[0].chunkId).toBe('ch1')
    expect(inserts[0].content).toBe('Hello')
  })

  it('fires callback on collection update', async () => {
    // Insert first
    controllers.promptTurns.emit([makeTurn('t1', 'queued', 0)])
    await flushPromises()

    // Subscribe after insert — track all change types
    const received: Array<{ type: string; value: PromptTurnRow }> = []
    db.collections.promptTurns.subscribeChanges((changes) => {
      for (const c of changes) {
        received.push({ type: c.type, value: c.value })
      }
    })

    // Update the turn via sync API
    controllers.promptTurns.utils.begin()
    controllers.promptTurns.utils.write({
      type: 'update',
      value: makeTurn('t1', 'active'),
    })
    controllers.promptTurns.utils.commit()
    await flushPromises()

    expect(received.length).toBeGreaterThanOrEqual(1)
    const updated = received.find((r) => r.value.state === 'active')
    expect(updated).toBeDefined()
  })

  it('returns { unsubscribe } that stops callbacks', async () => {
    const inserts: ChunkRow[] = []
    const sub = db.collections.chunks.subscribeChanges((changes) => {
      for (const c of changes) {
        if (c.type === 'insert') inserts.push(c.value)
      }
    })

    controllers.chunks.emit([makeChunk('ch1', 't1', 'text', 'First', 0)])
    await flushPromises()
    expect(inserts.length).toBe(1)

    sub.unsubscribe()

    controllers.chunks.utils.begin()
    controllers.chunks.utils.write({
      type: 'insert',
      value: makeChunk('ch2', 't1', 'text', 'Second', 1),
    })
    controllers.chunks.utils.commit()
    await flushPromises()

    // Should not have received the second insert
    expect(inserts.length).toBe(1)
  })

  it('does not fire for changes before subscription', async () => {
    // Insert data before subscribing
    controllers.promptTurns.emit([makeTurn('t1', 'queued', 0)])
    await flushPromises()

    const inserts: PromptTurnRow[] = []
    db.collections.promptTurns.subscribeChanges((changes) => {
      for (const c of changes) {
        if (c.type === 'insert') inserts.push(c.value)
      }
    })

    // No new changes — subscription should not have fired
    await flushPromises()
    expect(inserts.length).toBe(0)

    // Now insert new data — should fire
    controllers.promptTurns.utils.begin()
    controllers.promptTurns.utils.write({
      type: 'insert',
      value: makeTurn('t2', 'queued', 1),
    })
    controllers.promptTurns.utils.commit()
    await flushPromises()

    expect(inserts.length).toBe(1)
    expect(inserts[0].promptTurnId).toBe('t2')
  })
})

// ============================================================================
// Test Data Factories
// ============================================================================

function makeTurn(
  id: string,
  state: PromptTurnRow['state'],
  position?: number
): PromptTurnRow {
  return {
    promptTurnId: id,
    logicalConnectionId: 'c1',
    sessionId: 's1',
    requestId: id,
    text: `prompt-${id}`,
    state,
    position,
    startedAt: 0,
  }
}

function makePermission(
  id: string,
  state: PermissionRow['state']
): PermissionRow {
  return {
    requestId: id,
    jsonrpcId: 0,
    logicalConnectionId: 'c1',
    sessionId: 's1',
    promptTurnId: 't1',
    state,
    createdAt: Date.now(),
  }
}

function makeChunk(
  chunkId: string,
  promptTurnId: string,
  type: ChunkRow['type'],
  content: string,
  seq: number
): ChunkRow {
  return {
    chunkId,
    promptTurnId,
    logicalConnectionId: 'c1',
    type,
    content,
    seq,
    createdAt: Date.now(),
  }
}
