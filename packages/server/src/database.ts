import { PgClient, PgMigrator } from "@effect/sql-pg"
import { SqlClient } from "@effect/sql/SqlClient"
import { Context, Data, Duration, Effect, Layer } from "effect"
import { fileURLToPath } from "node:url"
import { AppConfigTag } from "./config.js"

export class DatabaseUnavailable extends Data.TaggedError("DatabaseUnavailable")<{
  readonly cause: unknown
}> {}

export interface DatabaseHealthShape {
  readonly check: Effect.Effect<void, DatabaseUnavailable>
}

export class DatabaseHealth extends Context.Tag("DatabaseHealth")<DatabaseHealth, DatabaseHealthShape>() {}

const migrationsDirectory = fileURLToPath(new URL("./migrations", import.meta.url))

export const PostgresLive = Layer.unwrapEffect(
  Effect.gen(function* () {
    const { postgresUrl } = yield* AppConfigTag
    return PgClient.layer({ url: postgresUrl })
  }),
)

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
          onTimeout: () => new DatabaseUnavailable({ cause: new Error("Database health check timed out") }),
        }),
      ),
    }
  }),
)

export const runDatabaseMigrations = PgMigrator.run({
  loader: PgMigrator.fromFileSystem(migrationsDirectory),
})
