import { HttpClient, HttpServer } from "@effect/platform"
import { PgClient } from "@effect/sql-pg"
import { PostgreSqlContainer } from "@testcontainers/postgresql"
import { describe, expect, it } from "@effect/vitest"
import { Data, Effect, Layer, Redacted } from "effect"
import { runDatabaseMigrations } from "../../src/database.js"
import { router } from "../../src/router.js"
import { makeServerTestLayer } from "./fixtures.js"

class ContainerError extends Data.TaggedError("ContainerError")<{
  readonly cause: unknown
}> {}

class PgContainer extends Effect.Service<PgContainer>()("test/PgContainer", {
  scoped: Effect.acquireRelease(
    Effect.tryPromise({
      try: () => new PostgreSqlContainer("postgres:18-alpine").start(),
      catch: (cause) => new ContainerError({ cause }),
    }),
    (container) => Effect.promise(() => container.stop()),
  ),
}) {
  static TestDatabaseLayer = Layer.unwrapEffect(
    Effect.gen(function* () {
      const container = yield* PgContainer
      return PgClient.layer({ url: Redacted.make(container.getConnectionUri()) })
    }),
  ).pipe(Layer.provide(this.Default))
}

const TestLayer = makeServerTestLayer(PgContainer.TestDatabaseLayer)

describe("postgres foundation", () => {
  it.scopedLive(
    "applies the baseline migration",
    () =>
      Effect.gen(function* () {
        const applied = yield* runDatabaseMigrations
        expect(applied).toEqual([
          [1, "postgres_foundation"],
          [2, "webhook_tables"],
        ])
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )

  it.scopedLive(
    "GET /health returns 200 while the server is running",
    () =>
      Effect.gen(function* () {
        yield* runDatabaseMigrations
        yield* router.pipe(HttpServer.serveEffect())
        const client = yield* HttpClient.HttpClient
        const res = yield* client.get("/health")
        expect(res.status).toBe(200)
        const body = yield* res.json
        expect(body).toEqual({ status: "ok" })
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )
})
