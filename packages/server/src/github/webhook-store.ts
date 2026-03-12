import { SqlClient, SqlError, SqlSchema } from "@effect/sql"
import { Context, Effect, Layer, Option, Schema } from "effect"
import type { ParseError } from "effect/ParseResult"

const RepoEntry = Schema.Struct({ full_name: Schema.String })

const InsertEventRequest = Schema.Struct({
  deliveryId: Schema.String,
  eventType: Schema.String,
  action: Schema.NullOr(Schema.String),
  repoFullName: Schema.NullOr(Schema.String),
  installationId: Schema.NullOr(Schema.Number),
  rawPayload: Schema.String,
})

export type InsertEventParams = typeof InsertEventRequest.Type

const InsertedRow = Schema.Struct({ id: Schema.UUID })

const UpsertInstallationRequest = Schema.Struct({
  installationId: Schema.Number,
  accountType: Schema.String,
  accountLogin: Schema.String,
  repos: Schema.NullOr(Schema.Array(RepoEntry)),
})

export type UpsertInstallationParams = typeof UpsertInstallationRequest.Type

const InstallationReposRow = Schema.Struct({
  repos: Schema.NullOr(Schema.Array(RepoEntry)),
})

type StoreError = SqlError.SqlError | ParseError

export interface WebhookStoreShape {
  readonly insertEvent: (params: InsertEventParams) => Effect.Effect<boolean, StoreError>
  readonly upsertInstallation: (params: UpsertInstallationParams) => Effect.Effect<void, StoreError>
  readonly getInstallationRepos: (
    installationId: number,
  ) => Effect.Effect<ReadonlyArray<typeof RepoEntry.Type> | null, StoreError>
  readonly deleteInstallation: (installationId: number) => Effect.Effect<void, StoreError>
}

export class WebhookStore extends Context.Tag("WebhookStore")<WebhookStore, WebhookStoreShape>() {}

export const WebhookStoreLive = Layer.effect(
  WebhookStore,
  Effect.gen(function* () {
    const sql = yield* SqlClient.SqlClient

    const _insertEvent = SqlSchema.findAll({
      Request: InsertEventRequest,
      Result: InsertedRow,
      execute: (p) =>
        sql`INSERT INTO webhook_events (delivery_id, event_type, action, repo_full_name, installation_id, payload)
            VALUES (${p.deliveryId}, ${p.eventType}, ${p.action}, ${p.repoFullName}, ${p.installationId}, ${p.rawPayload})
            ON CONFLICT (delivery_id) DO NOTHING
            RETURNING id`,
    })

    const _upsertInstallation = SqlSchema.void({
      Request: UpsertInstallationRequest,
      execute: (p) =>
        sql`INSERT INTO installations (installation_id, account_type, account_login, repos)
            VALUES (${p.installationId}, ${p.accountType}, ${p.accountLogin}, ${p.repos === null ? null : JSON.stringify(p.repos)})
            ON CONFLICT (installation_id) DO UPDATE SET
              account_type = EXCLUDED.account_type,
              account_login = EXCLUDED.account_login,
              repos = EXCLUDED.repos,
              updated_at = now()`,
    })

    const _getInstallationRepos = SqlSchema.findOne({
      Request: Schema.Number,
      Result: InstallationReposRow,
      execute: (installationId) =>
        sql`SELECT repos FROM installations WHERE installation_id = ${installationId}`,
    })

    const _deleteInstallation = SqlSchema.void({
      Request: Schema.Number,
      execute: (installationId) => sql`DELETE FROM installations WHERE installation_id = ${installationId}`,
    })

    return {
      insertEvent: (p) => _insertEvent(p).pipe(Effect.map((rows) => rows.length > 0)),
      upsertInstallation: _upsertInstallation,
      getInstallationRepos: (id) =>
        _getInstallationRepos(id).pipe(
          Effect.map(
            Option.match({
              onNone: () => null,
              onSome: (row) => row.repos,
            }),
          ),
        ),
      deleteInstallation: _deleteInstallation,
    }
  }),
)
