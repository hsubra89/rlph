import { NodeContext, NodeHttpServer } from "@effect/platform-node"
import { Effect, Layer } from "effect"
import { LoginRateLimiterLive } from "../../src/auth/login-rate-limiter.js"
import { ReplayGuardLive } from "../../src/auth/replay-guard.js"
import { TokenDenylistLive } from "../../src/auth/token-denylist.js"
import { JwtSecret } from "../../src/config.js"

export const TEST_JWT_SECRET = new TextEncoder().encode("test-secret-that-is-at-least-32-bytes-long")

export const TestJwtSecretLayer = Layer.succeed(JwtSecret, TEST_JWT_SECRET)

export const ServerTestLayer = Layer.mergeAll(
  NodeHttpServer.layerTest,
  NodeContext.layer,
  ReplayGuardLive,
  TokenDenylistLive,
  LoginRateLimiterLive,
  TestJwtSecretLayer,
)

export const makeServerTestLayer = (...layers: ReadonlyArray<Layer.Layer.Any>) => Layer.mergeAll(ServerTestLayer, ...layers)
