import { SqlClient } from "@effect/sql"
import { Effect } from "effect"

export default Effect.gen(function* () {
  const sql = yield* SqlClient.SqlClient
  yield* sql.unsafe(`
    CREATE TABLE webhook_events (
      id              UUID PRIMARY KEY DEFAULT uuidv7(),
      delivery_id     TEXT NOT NULL,
      event_type      TEXT NOT NULL,
      action          TEXT,
      repo_full_name  TEXT,
      installation_id BIGINT,
      payload         JSONB NOT NULL,
      received_at     TIMESTAMPTZ NOT NULL DEFAULT now()
    );
    CREATE UNIQUE INDEX idx_webhook_events_delivery_id ON webhook_events (delivery_id);
    CREATE INDEX idx_webhook_events_type_action ON webhook_events (event_type, action);
    CREATE INDEX idx_webhook_events_repo ON webhook_events (repo_full_name);
    CREATE INDEX idx_webhook_events_received ON webhook_events (received_at);

    CREATE TABLE installations (
      installation_id BIGINT PRIMARY KEY,
      account_type    TEXT NOT NULL,
      account_login   TEXT NOT NULL,
      repos           JSONB,
      created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
      updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
    );
  `)
})
