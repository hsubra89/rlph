import { FetchHttpClient, HttpServer } from "@effect/platform"
import { NodeContext, NodeHttpServer, NodeRuntime } from "@effect/platform-node"
import { Effect, Layer } from "effect"
import { createServer } from "node:http"
import { LoginRateLimiterLive } from "./auth/login-rate-limiter.js"
import { ReplayGuardLive } from "./auth/replay-guard.js"
import { TokenDenylistLive } from "./auth/token-denylist.js"
import { JwtSecretLive, ServerPort } from "./config.js"
import { DatabaseHealthLive, PostgresLive, PostgresMigrationsLive, runDatabaseMigrations } from "./database.js"
import { router } from "./router.js"

const program = Effect.gen(function* () {
  const port = yield* ServerPort

  const ServerLive = NodeHttpServer.layer(() => createServer(), { port })
  yield* Effect.logInfo("Running postgres migrations")
  const appliedMigrations = yield* runDatabaseMigrations.pipe(
    Effect.provide(PostgresMigrationsLive),
    Effect.provide(NodeContext.layer),
  )
  yield* Effect.logInfo(`Postgres migrations complete (${appliedMigrations.length} applied)`)

  const RuntimeDatabaseLive = DatabaseHealthLive.pipe(Layer.provideMerge(PostgresLive))

  const HttpLive = HttpServer.serve(router).pipe(
    Layer.provide(ServerLive),
    Layer.provide(NodeContext.layer),
    Layer.provide(RuntimeDatabaseLive),
    Layer.provide(ReplayGuardLive),
    Layer.provide(TokenDenylistLive),
    Layer.provide(LoginRateLimiterLive),
    Layer.provide(FetchHttpClient.layer),
    Layer.provide(JwtSecretLive),
  )

  yield* Effect.logInfo(`Server starting on port ${port}`)
  yield* Layer.launch(HttpLive)
})

NodeRuntime.runMain(program)
