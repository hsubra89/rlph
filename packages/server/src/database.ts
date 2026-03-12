import { PgClient, PgMigrator } from "@effect/sql-pg"
import { Config } from "effect"
import * as migration0001 from "./migrations/0001_postgres_foundation.js"
import * as migration0002 from "./migrations/0002_webhook_tables.js"
import * as migration0003 from "./migrations/0003_webhook_delivery_id.js"

export const PostgresLive = PgClient.layerConfig({
  url: Config.redacted("BRRR_POSTGRES_URL"),
})

export const PostgresMigrationsUrl = Config.redacted("BRRR_POSTGRES_MIGRATIONS_URL").pipe(
  Config.orElse(() => Config.redacted("BRRR_POSTGRES_URL")),
)

export const PostgresMigrationsLive = PgClient.layerConfig({
  url: PostgresMigrationsUrl,
})

export const runDatabaseMigrations = PgMigrator.run({
  loader: PgMigrator.fromRecord({
    "0001_postgres_foundation": migration0001.default,
    "0002_webhook_tables": migration0002.default,
    "0003_webhook_delivery_id": migration0003.default,
  }),
})
