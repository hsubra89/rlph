import { FetchHttpClient, HttpServer } from "@effect/platform"
import { NodeContext, NodeHttpServer, NodeRuntime } from "@effect/platform-node"
import { Config, Effect, Layer } from "effect"
import { createServer } from "node:http"
import { LoginRateLimiterLive } from "./auth/login-rate-limiter.js"
import { ReplayGuardLive } from "./auth/replay-guard.js"
import { TokenDenylistLive } from "./auth/token-denylist.js"
import { PostgresLive, PostgresMigrationsLive, runDatabaseMigrations } from "./database.js"
import { WebhookStoreLive } from "./github/webhook-store.js"
import { router } from "./router.js"

const ServerPort = Config.integer("BRRR_PORT").pipe(Config.withDefault(4000))

const program = Effect.gen(function* () {
  const port = yield* ServerPort

  const ServerLive = NodeHttpServer.layer(() => createServer(), { port })
  yield* Effect.logInfo("Running postgres migrations")
  const appliedMigrations = yield* runDatabaseMigrations.pipe(
    Effect.provide(PostgresMigrationsLive),
    Effect.provide(NodeContext.layer),
  )
  yield* Effect.logInfo(`Postgres migrations complete (${appliedMigrations.length} applied)`)

  const HttpLive = HttpServer.serve(router).pipe(
    Layer.provide(ServerLive),
    Layer.provide(NodeContext.layer),
    Layer.provide(ReplayGuardLive),
    Layer.provide(TokenDenylistLive),
    Layer.provide(LoginRateLimiterLive),
    Layer.provide(FetchHttpClient.layer),
    Layer.provide(WebhookStoreLive),
    Layer.provide(PostgresLive),
  )

  yield* Effect.logInfo(`Server starting on port ${port}`)
  yield* Layer.launch(HttpLive)
})

NodeRuntime.runMain(program)
