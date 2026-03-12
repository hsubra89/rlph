import { SqlClient } from "@effect/sql"
import { Effect } from "effect"

export default Effect.gen(function* () {
  const sql = yield* SqlClient.SqlClient
  yield* sql.unsafe(`
    ALTER TABLE webhook_events
      ADD COLUMN delivery_id TEXT NOT NULL;
    CREATE UNIQUE INDEX idx_webhook_events_delivery_id ON webhook_events (delivery_id);
  `)
})
