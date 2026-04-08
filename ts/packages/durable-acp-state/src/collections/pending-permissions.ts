import { createLiveQueryCollection } from '@tanstack/db'
import type { Collection } from '@tanstack/db'
import type { PermissionRow } from '../types'

export function createPendingPermissionsCollection(
  opts: { permissions: Collection<PermissionRow> },
): Collection<PermissionRow> {
  return createLiveQueryCollection({
    query: (q: any) => q
      .from({ p: opts.permissions })
      .fn.where(({ p }: { p: PermissionRow }) => p.state === 'pending'),
    getKey: (r: PermissionRow) => r.requestId,
  }) as unknown as Collection<PermissionRow>
}
