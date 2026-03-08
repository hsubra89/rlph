import { HttpRouter, HttpServer, HttpServerResponse } from "@effect/platform"
import { NodeHttpServer, NodeRuntime } from "@effect/platform-node"
import { Config, Effect, Layer, Redacted } from "effect"
import { createServer } from "node:http"
import { handleChallenge } from "./auth/challenge.js"
import { makeHandleVerify } from "./auth/verify.js"
import { makeAuthMiddleware } from "./auth/middleware.js"
import { handleWhoami } from "./auth/whoami.js"

const program = Effect.gen(function* () {
  const port = yield* Config.integer("BRRR_PORT").pipe(Config.withDefault(3000))
  const secret = yield* Config.redacted("BRRR_JWT_SECRET")
  const jwtSecret = new TextEncoder().encode(Redacted.value(secret))

  const authMiddleware = makeAuthMiddleware(jwtSecret)

  const router = HttpRouter.empty.pipe(
    HttpRouter.get("/health", HttpServerResponse.json({ status: "ok" })),
    HttpRouter.post("/auth/challenge", handleChallenge),
    HttpRouter.post("/auth/verify", makeHandleVerify(jwtSecret)),
    HttpRouter.get("/whoami", authMiddleware(handleWhoami)),
  )

  const ServerLive = NodeHttpServer.layer(() => createServer(), { port })
  const HttpLive = HttpServer.serve(router).pipe(Layer.provide(ServerLive))

  yield* Effect.logInfo(`Server starting on port ${port}`)
  yield* Layer.launch(HttpLive)
})

NodeRuntime.runMain(program)
