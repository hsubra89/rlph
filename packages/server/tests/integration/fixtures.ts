import { NodeContext, NodeHttpServer } from "@effect/platform-node"
import { PgClient } from "@effect/sql-pg"
import { PostgreSqlContainer } from "@testcontainers/postgresql"
import { ConfigProvider, Data, Effect, Layer, Redacted } from "effect"
import { LoginRateLimiterLive } from "../../src/auth/login-rate-limiter.js"
import { ReplayGuardLive } from "../../src/auth/replay-guard.js"
import { TokenDenylistLive } from "../../src/auth/token-denylist.js"
import { JwtSecret } from "../../src/config.js"
import { WebhookStore } from "../../src/github/webhook-store.js"
import { TEST_JWT_SECRET } from "../helpers/constants.js"

export { TEST_JWT_SECRET }

export const TEST_WEBHOOK_SECRET = "test-stub-secret"

export class ContainerError extends Data.TaggedError("ContainerError")<{
  readonly cause: unknown
}> {}

export class PgContainer extends Effect.Service<PgContainer>()("test/PgContainer", {
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

export const TestJwtSecretLayer = Layer.succeed(JwtSecret, TEST_JWT_SECRET)

const StubWebhookStoreLayer = Layer.succeed(WebhookStore, {
  insertEvent: () => Effect.succeed(true),
  upsertInstallation: () => Effect.void,
  getInstallationRepos: () => Effect.succeed({ found: true as const, repos: null }),
  deleteInstallation: () => Effect.void,
})

const TestConfigLayer = Layer.setConfigProvider(
  ConfigProvider.fromMap(new Map([["BRRR_GITHUB_WEBHOOK_SECRET", "test-stub-secret"]])),
)

export const ServerTestLayer = Layer.mergeAll(
  NodeHttpServer.layerTest,
  NodeContext.layer,
  ReplayGuardLive,
  TokenDenylistLive,
  LoginRateLimiterLive,
  TestJwtSecretLayer,
  StubWebhookStoreLayer,
  TestConfigLayer,
)

export const makeServerTestLayer = (...layers: ReadonlyArray<Layer.Layer<never, any, any>>) =>
  Layer.mergeAll(ServerTestLayer, ...layers)
