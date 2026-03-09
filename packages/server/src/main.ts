import { FetchHttpClient, HttpRouter, HttpServer, HttpServerResponse } from "@effect/platform"
import { NodeCommandExecutor, NodeFileSystem, NodeHttpServer, NodeRuntime } from "@effect/platform-node"
import { Config, Effect, Layer, Redacted } from "effect"
import { createServer } from "node:http"
import { makeHandleLogin } from "./auth/login.js"
import { LoginRateLimiterLive } from "./auth/login-rate-limiter.js"
import { makeAuthMiddleware } from "./auth/middleware.js"
import { ReplayGuardLive } from "./auth/replay-guard.js"
import { handleRevoke } from "./auth/revoke.js"
import { TokenDenylistLive } from "./auth/token-denylist.js"
import { handleWhoami } from "./auth/whoami.js"

const program = Effect.gen(function* () {
  const port = yield* Config.integer("BRRR_PORT").pipe(Config.withDefault(3000))
  const secret = yield* Config.redacted("BRRR_JWT_SECRET")
  const jwtSecret = new TextEncoder().encode(Redacted.value(secret))

  const authMiddleware = makeAuthMiddleware(jwtSecret)

  const router = HttpRouter.empty.pipe(
    HttpRouter.get("/health", HttpServerResponse.json({ status: "ok" })),
    HttpRouter.post("/auth/login", makeHandleLogin(jwtSecret)),
    HttpRouter.get("/whoami", authMiddleware(handleWhoami)),
    HttpRouter.post("/auth/revoke", authMiddleware(handleRevoke)),
  )

  const ServerLive = NodeHttpServer.layer(() => createServer(), { port })
  const PlatformLive = NodeCommandExecutor.layer.pipe(Layer.provideMerge(NodeFileSystem.layer))

  const HttpLive = HttpServer.serve(router).pipe(
    Layer.provide(ServerLive),
    Layer.provide(PlatformLive),
    Layer.provide(ReplayGuardLive),
    Layer.provide(TokenDenylistLive),
    Layer.provide(LoginRateLimiterLive),
    Layer.provide(FetchHttpClient.layer),
  )

  yield* Effect.logInfo(`Server starting on port ${port}`)
  yield* Layer.launch(HttpLive)
})

NodeRuntime.runMain(program)
