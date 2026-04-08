/**
 * Re-export mock helpers from @durable-acp/state for client testing.
 */

import { createCollection } from "@tanstack/db";
import type { Collection, SyncConfig, ChangeMessage } from "@tanstack/db";
import type {
  ConnectionRow,
  PromptTurnRow,
  PendingRequestRow,
  PermissionRow,
  TerminalRow,
  RuntimeInstanceRow,
  ChunkRow,
  DurableACPDB,
} from "@durable-acp/state";

export interface MockCollectionController<T extends object> {
  emit: (rows: T[]) => void;
  markReady: () => void;
  utils: {
    begin: () => void;
    write: (change: ChangeMessage<T>) => void;
    commit: () => void;
    markReady: () => void;
  };
}

function createMockCollection<T extends object>(
  id: string,
  getKey: (row: T) => string,
): {
  collection: Collection<T>;
  controller: MockCollectionController<T>;
} {
  let begin!: () => void;
  let write!: (change: ChangeMessage<T>) => void;
  let commit!: () => void;
  let markReadyFn!: () => void;

  const sync: SyncConfig<T>["sync"] = (params) => {
    begin = params.begin;
    write = params.write;
    commit = params.commit;
    markReadyFn = params.markReady;
  };

  const collection = createCollection<T>({
    id,
    getKey,
    sync: { sync },
  });

  const controller: MockCollectionController<T> = {
    emit: (rows: T[]) => {
      begin();
      for (const row of rows) {
        write({ type: "insert", value: row });
      }
      commit();
    },
    markReady: () => markReadyFn(),
    utils: {
      begin: () => begin(),
      write: (change) => write(change),
      commit: () => commit(),
      markReady: () => markReadyFn(),
    },
  };

  return { collection, controller };
}

export interface MockDurableACPDBControllers {
  connections: MockCollectionController<ConnectionRow>;
  promptTurns: MockCollectionController<PromptTurnRow>;
  pendingRequests: MockCollectionController<PendingRequestRow>;
  permissions: MockCollectionController<PermissionRow>;
  terminals: MockCollectionController<TerminalRow>;
  runtimeInstances: MockCollectionController<RuntimeInstanceRow>;
  chunks: MockCollectionController<ChunkRow>;
}

export function createMockDurableACPDB(): {
  db: DurableACPDB;
  controllers: MockDurableACPDBControllers;
} {
  const { collection: connections, controller: connectionsCtrl } =
    createMockCollection<ConnectionRow>(
      "test-connections",
      (row) => row.logicalConnectionId,
    );
  const { collection: promptTurns, controller: promptTurnsCtrl } =
    createMockCollection<PromptTurnRow>(
      "test-promptTurns",
      (row) => row.promptTurnId,
    );
  const { collection: pendingRequests, controller: pendingRequestsCtrl } =
    createMockCollection<PendingRequestRow>(
      "test-pendingRequests",
      (row) => row.requestId,
    );
  const { collection: permissions, controller: permissionsCtrl } =
    createMockCollection<PermissionRow>(
      "test-permissions",
      (row) => row.requestId,
    );
  const { collection: terminals, controller: terminalsCtrl } =
    createMockCollection<TerminalRow>(
      "test-terminals",
      (row) => row.terminalId,
    );
  const { collection: runtimeInstances, controller: runtimeInstancesCtrl } =
    createMockCollection<RuntimeInstanceRow>(
      "test-runtimeInstances",
      (row) => row.instanceId,
    );
  const { collection: chunks, controller: chunksCtrl } =
    createMockCollection<ChunkRow>("test-chunks", (row) => row.chunkId);

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
      connections.preload();
      promptTurns.preload();
      pendingRequests.preload();
      permissions.preload();
      terminals.preload();
      runtimeInstances.preload();
      chunks.preload();
      connectionsCtrl.markReady();
      promptTurnsCtrl.markReady();
      pendingRequestsCtrl.markReady();
      permissionsCtrl.markReady();
      terminalsCtrl.markReady();
      runtimeInstancesCtrl.markReady();
      chunksCtrl.markReady();
    },
    close: () => {},
    utils: { awaitTxId: async () => {} },
  } as unknown as DurableACPDB;

  return {
    db,
    controllers: {
      connections: connectionsCtrl,
      promptTurns: promptTurnsCtrl,
      pendingRequests: pendingRequestsCtrl,
      permissions: permissionsCtrl,
      terminals: terminalsCtrl,
      runtimeInstances: runtimeInstancesCtrl,
      chunks: chunksCtrl,
    },
  };
}

export async function flushPromises(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 40));
}
