import { HttpServerRequest, HttpServerResponse } from "@effect/platform"
import { Effect } from "effect"
import { GitHubWebhookSecret } from "./config.js"
import { verifyWebhookSignature } from "./webhook-signature.js"
import { WebhookStore } from "./webhook-store.js"

const INSTALLATION_EVENTS = new Set(["installation", "installation_repositories"])

export const handleWebhook = Effect.gen(function* () {
  const request = yield* HttpServerRequest.HttpServerRequest
  const secret = yield* GitHubWebhookSecret
  const store = yield* WebhookStore

  const rawBody = yield* request.text

  const signatureHeader = request.headers["x-hub-signature-256"]
  if (!signatureHeader) {
    return yield* HttpServerResponse.json({ error: "missing signature" }, { status: 401 })
  }

  const verifyResult = yield* verifyWebhookSignature(secret, signatureHeader, rawBody).pipe(Effect.either)
  if (verifyResult._tag === "Left") {
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

  const insertResult = yield* store
    .insertEvent({ eventType, action, repoFullName, installationId, payload })
    .pipe(Effect.either)
  if (insertResult._tag === "Left") {
    return yield* HttpServerResponse.json({ error: "internal error" }, { status: 500 })
  }

  if (INSTALLATION_EVENTS.has(eventType) && installationId !== null) {
    const account = payload.installation
    yield* store
      .upsertInstallation({
        installationId,
        accountType: account?.account?.type ?? "Unknown",
        accountLogin: account?.account?.login ?? "unknown",
        repos: payload.repositories ?? payload.repositories_added ?? null,
      })
      .pipe(Effect.catchAll(() => Effect.void))
  }

  return yield* HttpServerResponse.json({ received: true })
})
