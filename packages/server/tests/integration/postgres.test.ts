import { HttpClient, HttpServer } from "@effect/platform"
import { NodeContext, NodeHttpServer } from "@effect/platform-node"
import { PgClient } from "@effect/sql-pg"
import { PostgreSqlContainer } from "@testcontainers/postgresql"
import { describe, expect, it } from "@effect/vitest"
import { Data, Effect, Layer, Redacted } from "effect"
import { LoginRateLimiterLive } from "../../src/auth/login-rate-limiter.js"
import { ReplayGuardLive } from "../../src/auth/replay-guard.js"
import { TokenDenylistLive } from "../../src/auth/token-denylist.js"
import { JwtSecret } from "../../src/config.js"
import { DatabaseHealthLive, runDatabaseMigrations } from "../../src/database.js"
import { router } from "../../src/router.js"

const JWT_SECRET = new TextEncoder().encode("test-secret-that-is-at-least-32-bytes-long")

export class ContainerError extends Data.TaggedError("ContainerError")<{
  readonly cause: unknown
}> {}

class PgContainer extends Effect.Service<PgContainer>()("test/PgContainer", {
  scoped: Effect.acquireRelease(
    Effect.tryPromise({
      try: () => new PostgreSqlContainer("postgres:alpine").start(),
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

const TestJwtSecretLayer = Layer.succeed(JwtSecret, JWT_SECRET)

const DatabaseRuntimeLive = DatabaseHealthLive.pipe(Layer.provide(PgContainer.TestDatabaseLayer))

const TestLayer = Layer.mergeAll(
  NodeContext.layer,
  NodeHttpServer.layerTest,
  ReplayGuardLive,
  TokenDenylistLive,
  LoginRateLimiterLive,
  DatabaseRuntimeLive,
  PgContainer.TestDatabaseLayer,
  TestJwtSecretLayer,
)

describe("postgres foundation", () => {
  it.scopedLive(
    "applies the baseline migration",
    () =>
      Effect.gen(function* () {
        const applied = yield* runDatabaseMigrations
        expect(applied).toEqual([[1, "postgres_foundation"]])
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )

  it.scopedLive(
    "GET /health returns 200 with a live postgres connection",
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
