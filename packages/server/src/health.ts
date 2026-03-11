import { HttpServerResponse } from "@effect/platform"
import { Effect } from "effect"

export const handleHealth = Effect.gen(function* () {
  return yield* HttpServerResponse.json({ status: "ok" })
})
