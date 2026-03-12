import { HttpServerRequest, HttpServerResponse } from "@effect/platform"
import { Config, Effect, Either, Redacted } from "effect"
import { verifyWebhookSignature } from "./webhook-signature.js"
import { WebhookStore } from "./webhook-store.js"
import { SqlClient } from "@effect/sql"

const INSTALLATION_EVENTS = new Set(["installation", "installation_repositories"])

export const handleWebhook = Effect.gen(function* () {
  const request = yield* HttpServerRequest.HttpServerRequest
  const secret = Redacted.value(yield* Config.redacted("BRRR_GITHUB_WEBHOOK_SECRET"))
  const store = yield* WebhookStore

  const rawBody = yield* request.text

  const signatureHeader = request.headers["x-hub-signature-256"]

  if (!signatureHeader) {
    return yield* HttpServerResponse.json({ error: "missing signature" }, { status: 401 })
  }

  const verifyResult = yield* Effect.either(verifyWebhookSignature(secret, signatureHeader, rawBody))

  if (Either.isLeft(verifyResult)) {
    return yield* HttpServerResponse.json({ error: "invalid signature" }, { status: 401 })
  }

  const eventType = request.headers["x-github-event"]
  if (!eventType) {
    return yield* HttpServerResponse.json({ error: "missing event type" }, { status: 400 })
  }

  const payload = JSON.parse(rawBody)
  const action: string | null = payload.action ?? null
  const repoFullName: string | null = payload.repository?.full_name ?? null
  const installationId: number | null = payload.installation?.id ?? null

  const sql = yield* SqlClient.SqlClient

  const persist = sql.withTransaction(
    Effect.gen(function* () {
      yield* store.insertEvent({ eventType, action, repoFullName, installationId, rawPayload: rawBody })

      if (INSTALLATION_EVENTS.has(eventType) && installationId !== null) {
        const account = payload.installation
        yield* store.upsertInstallation({
          installationId,
          accountType: account?.account?.type ?? "Unknown",
          accountLogin: account?.account?.login ?? "unknown",
          repos: payload.repositories ?? payload.repositories_added ?? null,
        })
      }
    }),
  )

  const persistResult = yield* Effect.either(persist)
  if (Either.isLeft(persistResult)) {
    return yield* HttpServerResponse.json({ error: "internal error" }, { status: 500 })
  }

  return yield* HttpServerResponse.json({ received: true })
})
