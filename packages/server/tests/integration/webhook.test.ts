import { HttpBody, HttpClient, HttpServer } from "@effect/platform"
import { SqlClient } from "@effect/sql"
import { describe, expect, it } from "@effect/vitest"
import { Effect, Layer } from "effect"
import * as crypto from "node:crypto"
import { runDatabaseMigrations } from "../../src/database.js"
import { GitHubWebhookSecret } from "../../src/github/config.js"
import { WebhookStoreLive } from "../../src/github/webhook-store.js"
import { router } from "../../src/router.js"
import { signPayload } from "../helpers/webhook.js"
import { makeServerTestLayer, PgContainer } from "./fixtures.js"

const TEST_WEBHOOK_SECRET = "test-webhook-secret-for-integration"

const TestGitHubWebhookSecretLayer = Layer.succeed(GitHubWebhookSecret, TEST_WEBHOOK_SECRET)

const DbLayer = PgContainer.TestDatabaseLayer

const TestLayer = makeServerTestLayer(
  DbLayer,
  WebhookStoreLive.pipe(Layer.provide(DbLayer)),
  TestGitHubWebhookSecretLayer,
)

function webhookHeaders(body: string, eventType: string, opts?: { skipSignature?: boolean }) {
  const headers: Record<string, string> = {
    "x-github-event": eventType,
    "x-github-delivery": crypto.randomUUID(),
  }
  if (!opts?.skipSignature) {
    headers["x-hub-signature-256"] = signPayload(TEST_WEBHOOK_SECRET, body)
  }
  return headers
}

describe("webhook receiver", () => {
  it.scopedLive(
    "valid webhook → 200, event row in DB",
    () =>
      Effect.gen(function* () {
        yield* runDatabaseMigrations
        yield* router.pipe(HttpServer.serveEffect())
        const client = yield* HttpClient.HttpClient

        const body = JSON.stringify({
          action: "opened",
          repository: { full_name: "owner/repo" },
          installation: { id: 12345 },
        })

        const res = yield* client.post("/webhooks/github", {
          body: HttpBody.text(body, "application/json"),
          headers: webhookHeaders(body, "pull_request"),
        })
        expect(res.status).toBe(200)
        const resBody = yield* res.json
        expect(resBody).toEqual({ received: true })

        // Verify DB row
        const sql = yield* SqlClient.SqlClient
        const rows = yield* sql.unsafe(
          `SELECT event_type, action, repo_full_name, installation_id, received_at FROM webhook_events`,
        )
        expect(rows.length).toBe(1)
        expect(rows[0].event_type).toBe("pull_request")
        expect(rows[0].action).toBe("opened")
        expect(rows[0].repo_full_name).toBe("owner/repo")
        expect(String(rows[0].installation_id)).toBe("12345")
        expect(rows[0].received_at).toBeInstanceOf(Date)

        // Verify payload JSONB is queryable
        const jsonbRows = yield* sql.unsafe(
          `SELECT payload->>'action' AS action FROM webhook_events WHERE payload->>'action' = 'opened'`,
        )
        expect(jsonbRows.length).toBe(1)
        expect(jsonbRows[0].action).toBe("opened")
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )

  it.scopedLive(
    "invalid signature → 401",
    () =>
      Effect.gen(function* () {
        yield* runDatabaseMigrations
        yield* router.pipe(HttpServer.serveEffect())
        const client = yield* HttpClient.HttpClient

        const body = JSON.stringify({ action: "opened" })

        const res = yield* client.post("/webhooks/github", {
          body: HttpBody.text(body, "application/json"),
          headers: {
            "x-github-event": "push",
            "x-github-delivery": crypto.randomUUID(),
            "x-hub-signature-256": signPayload("wrong-secret", body),
          },
        })
        expect(res.status).toBe(401)
        const resBody = yield* res.json
        expect(resBody).toEqual({ error: "invalid signature" })
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )

  it.scopedLive(
    "missing signature header → 401",
    () =>
      Effect.gen(function* () {
        yield* runDatabaseMigrations
        yield* router.pipe(HttpServer.serveEffect())
        const client = yield* HttpClient.HttpClient

        const body = JSON.stringify({ action: "opened" })

        const res = yield* client.post("/webhooks/github", {
          body: HttpBody.text(body, "application/json"),
          headers: webhookHeaders(body, "push", { skipSignature: true }),
        })
        expect(res.status).toBe(401)
        const resBody = yield* res.json
        expect(resBody).toEqual({ error: "missing signature" })
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )

  it.scopedLive(
    "missing event type header → 400",
    () =>
      Effect.gen(function* () {
        yield* runDatabaseMigrations
        yield* router.pipe(HttpServer.serveEffect())
        const client = yield* HttpClient.HttpClient

        const body = JSON.stringify({ action: "opened" })

        const res = yield* client.post("/webhooks/github", {
          body: HttpBody.text(body, "application/json"),
          headers: {
            "x-hub-signature-256": signPayload(TEST_WEBHOOK_SECRET, body),
            "x-github-delivery": crypto.randomUUID(),
          },
        })
        expect(res.status).toBe(400)
        const resBody = yield* res.json
        expect(resBody).toEqual({ error: "missing event type" })
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )

  it.scopedLive(
    "installation event → upserts installations row",
    () =>
      Effect.gen(function* () {
        yield* runDatabaseMigrations
        yield* router.pipe(HttpServer.serveEffect())
        const client = yield* HttpClient.HttpClient

        const body = JSON.stringify({
          action: "created",
          installation: {
            id: 99999,
            account: { type: "Organization", login: "my-org" },
          },
          repositories: [{ full_name: "my-org/repo1" }],
        })

        const res = yield* client.post("/webhooks/github", {
          body: HttpBody.text(body, "application/json"),
          headers: webhookHeaders(body, "installation"),
        })
        expect(res.status).toBe(200)

        // Verify installations table
        const sql = yield* SqlClient.SqlClient
        const rows = yield* sql.unsafe(
          `SELECT installation_id, account_type, account_login, repos FROM installations WHERE installation_id = 99999`,
        )
        expect(rows.length).toBe(1)
        expect(rows[0].account_type).toBe("Organization")
        expect(rows[0].account_login).toBe("my-org")
        expect(rows[0].repos).toEqual([{ full_name: "my-org/repo1" }])
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )

  it.scopedLive(
    "installation_repositories event → updates repos list",
    () =>
      Effect.gen(function* () {
        yield* runDatabaseMigrations
        yield* router.pipe(HttpServer.serveEffect())
        const client = yield* HttpClient.HttpClient

        // First: create installation
        const createBody = JSON.stringify({
          action: "created",
          installation: {
            id: 88888,
            account: { type: "User", login: "someuser" },
          },
          repositories: [{ full_name: "someuser/repo1" }],
        })
        yield* client.post("/webhooks/github", {
          body: HttpBody.text(createBody, "application/json"),
          headers: webhookHeaders(createBody, "installation"),
        })

        // Second: repos added event
        const reposBody = JSON.stringify({
          action: "added",
          installation: {
            id: 88888,
            account: { type: "User", login: "someuser" },
          },
          repositories_added: [{ full_name: "someuser/repo2" }],
        })
        const res = yield* client.post("/webhooks/github", {
          body: HttpBody.text(reposBody, "application/json"),
          headers: webhookHeaders(reposBody, "installation_repositories"),
        })
        expect(res.status).toBe(200)

        // Verify updated repos
        const sql = yield* SqlClient.SqlClient
        const rows = yield* sql.unsafe(`SELECT repos FROM installations WHERE installation_id = 88888`)
        expect(rows.length).toBe(1)
        expect(rows[0].repos).toEqual([{ full_name: "someuser/repo2" }])
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )
})
