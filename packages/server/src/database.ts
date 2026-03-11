import { PgClient, PgMigrator } from "@effect/sql-pg"
import { SqlClient } from "@effect/sql/SqlClient"
import { Config, Context, Data, Duration, Effect, Layer } from "effect"
import * as migration0001 from "./migrations/0001_postgres_foundation.js"

export class DatabaseUnavailable extends Data.TaggedError("DatabaseUnavailable")<{
  readonly cause: unknown
}> {}

export class DatabaseHealth extends Context.Tag("DatabaseHealth")<
  DatabaseHealth,
  { readonly check: Effect.Effect<void, DatabaseUnavailable> }
>() {}

export const PostgresLive = PgClient.layerConfig({
  url: Config.redacted("BRRR_POSTGRES_URL"),
})

export const PostgresMigrationsUrl = Config.redacted("BRRR_POSTGRES_MIGRATIONS_URL").pipe(
  Config.orElse(() => Config.redacted("BRRR_POSTGRES_URL")),
)

export const PostgresMigrationsLive = PgClient.layerConfig({
  url: PostgresMigrationsUrl,
})

export const DatabaseHealthLive = Layer.effect(
  DatabaseHealth,
  Effect.gen(function* () {
    const sql = yield* SqlClient

    return {
      check: sql`SELECT 1`.pipe(
        Effect.asVoid,
        Effect.mapError((cause) => new DatabaseUnavailable({ cause })),
        Effect.timeoutFail({
          duration: Duration.seconds(2),
          onTimeout: () => new DatabaseUnavailable({ cause: new Error("database health check timed out") }),
        }),
      ),
    }
  }),
)

export const runDatabaseMigrations = PgMigrator.run({
  loader: PgMigrator.fromRecord({
    "0001_postgres_foundation": migration0001.default,
  }),
})
