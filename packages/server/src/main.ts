import { FetchHttpClient, HttpServer } from "@effect/platform"
import { NodeCommandExecutor, NodeFileSystem, NodeHttpServer, NodeRuntime } from "@effect/platform-node"
import { Effect, Layer } from "effect"
import { createServer } from "node:http"
import { LoginRateLimiterLive } from "./auth/login-rate-limiter.js"
import { ReplayGuardLive } from "./auth/replay-guard.js"
import { TokenDenylistLive } from "./auth/token-denylist.js"
import { AppConfigTag, AppConfigLiveLayer } from "./config.js"
import { router } from "./router.js"

const program = Effect.gen(function* () {
  const { port } = yield* AppConfigTag

  const ServerLive = NodeHttpServer.layer(() => createServer(), { port })
  const PlatformLive = NodeCommandExecutor.layer.pipe(Layer.provideMerge(NodeFileSystem.layer))

  const HttpLive = HttpServer.serve(router).pipe(
    Layer.provide(ServerLive),
    Layer.provide(PlatformLive),
    Layer.provide(ReplayGuardLive),
    Layer.provide(TokenDenylistLive),
    Layer.provide(LoginRateLimiterLive),
    Layer.provide(FetchHttpClient.layer),
    Layer.provide(AppConfigLiveLayer),
  )

  yield* Effect.logInfo(`Server starting on port ${port}`)
  yield* Layer.launch(HttpLive)
})

NodeRuntime.runMain(program.pipe(Effect.provide(AppConfigLiveLayer)))
