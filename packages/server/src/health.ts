import { HttpServerResponse } from "@effect/platform"
import { Effect, Either } from "effect"
import { DatabaseHealth } from "./database.js"

export const handleHealth = Effect.gen(function* () {
  const databaseHealth = yield* DatabaseHealth
  const healthCheck = yield* Effect.either(databaseHealth.check)

  if (Either.isLeft(healthCheck)) {
    return yield* HttpServerResponse.json({ status: "database_unavailable" }, { status: 503 })
  }

  return yield* HttpServerResponse.json({ status: "ok" })
})
