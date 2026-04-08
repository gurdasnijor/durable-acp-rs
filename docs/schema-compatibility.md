# Schema Compatibility: Rust ↔ TypeScript

> Verified 2026-04-07. The Rust conductor's STATE-PROTOCOL output is
> **compatible** with the TypeScript `@durable-acp/state` StreamDB schema.

## Verification Method

Field-by-field comparison of:
- Rust: `src/state.rs` (serde serialization with `#[serde(rename_all = "camelCase")]`)
- TypeScript: `distributed-acp/packages/durable-acp-state/src/schema.ts` (Zod validators)

## Results

| Collection | Entity Type | Fields | Enums | Compatible |
|---|---|---|---|---|
| connections | `connection` | 8/8 match | 4/4 match | ✅ |
| promptTurns | `prompt_turn` | 10/10 match | 7/7 match | ✅ |
| chunks | `chunk` | 7/7 match | 6/6 match | ✅ |
| permissions | `permission` | 12/12 match | 3/3 match | ✅ |
| terminals | `terminal` | 10/10 match | 4/4 match | ✅ |
| pendingRequests | `pending_request` | 9/9 match | 5/5 match | ✅ |
| runtimeInstances | `runtime_instance` | 5/5 match | 3/3 match | ✅ |

## One Additive Difference

`ConnectionRow` in Rust has `cwd: Option<String>` which the TS schema
doesn't include. This is additive — Zod's default behavior strips
unknown fields. To expose it in the TS client, add to the schema:

```typescript
// In @durable-acp/state schema.ts, connections schema:
cwd: z.string().optional(),
```

## What This Means

The TypeScript `DurableACPClient` from `@durable-acp/client` can
subscribe to a Rust conductor's durable stream and it will work
out of the box. No adapter, no mapping, no conversion.

```typescript
const db = createDurableACPDB({
  stateStreamUrl: "http://localhost:4437/streams/durable-acp-state"
});
await db.preload();
// All collections populated correctly from Rust conductor's events
```

## STATE-PROTOCOL Headers

Both implementations use the same envelope:

```json
{
  "headers": { "operation": "insert", "type": "connection" },
  "key": "uuid-here",
  "value": { "logicalConnectionId": "...", "state": "created", ... }
}
```

Entity type strings match exactly:
`connection`, `prompt_turn`, `chunk`, `permission`, `terminal`,
`pending_request`, `runtime_instance`.
