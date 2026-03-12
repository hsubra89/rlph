import { SqlClient, SqlError } from "@effect/sql"
import { Context, Effect, Layer } from "effect"

export interface InsertEventParams {
  readonly deliveryId: string
  readonly eventType: string
  readonly action: string | null
  readonly repoFullName: string | null
  readonly installationId: number | null
  readonly rawPayload: string
}

export interface UpsertInstallationParams {
  readonly installationId: number
  readonly accountType: string
  readonly accountLogin: string
  readonly repos: unknown
}

export interface WebhookStoreShape {
  readonly insertEvent: (params: InsertEventParams) => Effect.Effect<boolean, SqlError.SqlError>
  readonly upsertInstallation: (params: UpsertInstallationParams) => Effect.Effect<void, SqlError.SqlError>
}

export class WebhookStore extends Context.Tag("WebhookStore")<WebhookStore, WebhookStoreShape>() {}

export const WebhookStoreLive = Layer.effect(
  WebhookStore,
  Effect.gen(function* () {
    const sql = yield* SqlClient.SqlClient

    return {
      insertEvent: (p: InsertEventParams) =>
        sql`INSERT INTO webhook_events (delivery_id, event_type, action, repo_full_name, installation_id, payload)
            VALUES (${p.deliveryId}, ${p.eventType}, ${p.action}, ${p.repoFullName}, ${p.installationId}, ${p.rawPayload})
            ON CONFLICT (delivery_id) DO NOTHING
            RETURNING id`.pipe(Effect.map((rows) => rows.length > 0)),

      upsertInstallation: (p: UpsertInstallationParams) =>
        sql`INSERT INTO installations (installation_id, account_type, account_login, repos)
            VALUES (${p.installationId}, ${p.accountType}, ${p.accountLogin}, ${JSON.stringify(p.repos)})
            ON CONFLICT (installation_id) DO UPDATE SET
              account_type = EXCLUDED.account_type,
              account_login = EXCLUDED.account_login,
              repos = EXCLUDED.repos,
              updated_at = now()`.pipe(Effect.asVoid),
    }
  }),
)
