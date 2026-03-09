import { HttpServerResponse } from "@effect/platform"
import { Effect } from "effect"
import { DatabaseHealth } from "./database.js"

export const handleHealth = Effect.gen(function* () {
  const database = yield* DatabaseHealth

  return yield* database.check.pipe(
    Effect.matchEffect({
      onFailure: () => HttpServerResponse.json({ error: "database unavailable" }, { status: 503 }),
      onSuccess: () => HttpServerResponse.json({ status: "ok" }),
    }),
  )
})
