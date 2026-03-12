import { HttpBody, HttpClient, HttpServer } from "@effect/platform"
import { SqlClient, SqlError } from "@effect/sql"
import { describe, expect, it } from "@effect/vitest"
import { Effect, Layer } from "effect"
import * as crypto from "node:crypto"
import { runDatabaseMigrations } from "../../src/database.js"
import { WebhookStore, WebhookStoreLive } from "../../src/github/webhook-store.js"
import { router } from "../../src/router.js"
import { signPayload } from "../helpers/webhook.js"
import { makeServerTestLayer, PgContainer, TEST_WEBHOOK_SECRET } from "./fixtures.js"

const DbLayer = PgContainer.TestDatabaseLayer

const TestLayer = makeServerTestLayer(DbLayer, WebhookStoreLive.pipe(Layer.provide(DbLayer)))

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
        const rows: readonly any[] = yield* sql.unsafe(
          `SELECT event_type, action, repo_full_name, installation_id, received_at FROM webhook_events`,
        )
        expect(rows.length).toBe(1)
        expect(rows[0].event_type).toBe("pull_request")
        expect(rows[0].action).toBe("opened")
        expect(rows[0].repo_full_name).toBe("owner/repo")
        expect(String(rows[0].installation_id)).toBe("12345")
        expect(rows[0].received_at).toBeInstanceOf(Date)

        // Verify payload JSONB is queryable
        const jsonbRows: readonly any[] = yield* sql.unsafe(
          `SELECT payload->>'action' AS action FROM webhook_events WHERE payload->>'action' = 'opened'`,
        )
        expect(jsonbRows.length).toBe(1)
        expect(jsonbRows[0].action).toBe("opened")
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )

  it.scopedLive(
    "duplicate delivery id → 200 idempotent, no second row or upsert",
    () =>
      Effect.gen(function* () {
        yield* runDatabaseMigrations
        yield* router.pipe(HttpServer.serveEffect())
        const client = yield* HttpClient.HttpClient

        const deliveryId = crypto.randomUUID()

        const body = JSON.stringify({
          action: "created",
          installation: {
            id: 55555,
            account: { type: "Organization", login: "dedup-org" },
          },
          repositories: [{ full_name: "dedup-org/repo1" }],
        })

        const headers = {
          "x-github-event": "installation",
          "x-github-delivery": deliveryId,
          "x-hub-signature-256": signPayload(TEST_WEBHOOK_SECRET, body),
        }

        // First delivery
        const res1 = yield* client.post("/webhooks/github", {
          body: HttpBody.text(body, "application/json"),
          headers,
        })
        expect(res1.status).toBe(200)

        // Second delivery with same delivery id
        const res2 = yield* client.post("/webhooks/github", {
          body: HttpBody.text(body, "application/json"),
          headers,
        })
        expect(res2.status).toBe(200)
        const resBody = yield* res2.json
        expect(resBody).toEqual({ received: true })

        // Verify only one event row
        const sql = yield* SqlClient.SqlClient
        const eventRows: readonly any[] = yield* sql.unsafe(
          `SELECT * FROM webhook_events WHERE delivery_id = '${deliveryId}'`,
        )
        expect(eventRows.length).toBe(1)

        // Verify installation was upserted only once (created_at == updated_at)
        const instRows: readonly any[] = yield* sql.unsafe(
          `SELECT created_at, updated_at FROM installations WHERE installation_id = 55555`,
        )
        expect(instRows.length).toBe(1)
        expect(instRows[0].created_at).toEqual(instRows[0].updated_at)
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
    "missing delivery id → 400",
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
            "x-github-event": "push",
          },
        })
        expect(res.status).toBe(400)
        const resBody = yield* res.json
        expect(resBody).toEqual({ error: "missing delivery id" })
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
        const rows: readonly any[] = yield* sql.unsafe(
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
    "installation_repositories:added → merges into existing repos",
    () =>
      Effect.gen(function* () {
        yield* runDatabaseMigrations
        yield* router.pipe(HttpServer.serveEffect())
        const client = yield* HttpClient.HttpClient

        // First: create installation with repo1
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

        // Second: repos added event adds repo2
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

        // Verify repos contains both repo1 and repo2
        const sql = yield* SqlClient.SqlClient
        const rows: readonly any[] = yield* sql.unsafe(
          `SELECT repos FROM installations WHERE installation_id = 88888`,
        )
        expect(rows.length).toBe(1)
        expect(rows[0].repos).toEqual([{ full_name: "someuser/repo1" }, { full_name: "someuser/repo2" }])
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )

  it.scopedLive(
    "installation_repositories:removed → removes from existing repos",
    () =>
      Effect.gen(function* () {
        yield* runDatabaseMigrations
        yield* router.pipe(HttpServer.serveEffect())
        const client = yield* HttpClient.HttpClient

        // Create installation with two repos
        const createBody = JSON.stringify({
          action: "created",
          installation: {
            id: 44444,
            account: { type: "User", login: "someuser" },
          },
          repositories: [{ full_name: "someuser/repo1" }, { full_name: "someuser/repo2" }],
        })
        yield* client.post("/webhooks/github", {
          body: HttpBody.text(createBody, "application/json"),
          headers: webhookHeaders(createBody, "installation"),
        })

        // Remove repo1
        const removeBody = JSON.stringify({
          action: "removed",
          installation: {
            id: 44444,
            account: { type: "User", login: "someuser" },
          },
          repositories_removed: [{ full_name: "someuser/repo1" }],
        })
        const res = yield* client.post("/webhooks/github", {
          body: HttpBody.text(removeBody, "application/json"),
          headers: webhookHeaders(removeBody, "installation_repositories"),
        })
        expect(res.status).toBe(200)

        // Verify only repo2 remains
        const sql = yield* SqlClient.SqlClient
        const rows: readonly any[] = yield* sql.unsafe(
          `SELECT repos FROM installations WHERE installation_id = 44444`,
        )
        expect(rows.length).toBe(1)
        expect(rows[0].repos).toEqual([{ full_name: "someuser/repo2" }])
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )

  it.scopedLive(
    "installation:deleted → removes installation row",
    () =>
      Effect.gen(function* () {
        yield* runDatabaseMigrations
        yield* router.pipe(HttpServer.serveEffect())
        const client = yield* HttpClient.HttpClient

        // Create installation
        const createBody = JSON.stringify({
          action: "created",
          installation: {
            id: 33333,
            account: { type: "Organization", login: "del-org" },
          },
          repositories: [{ full_name: "del-org/repo1" }],
        })
        yield* client.post("/webhooks/github", {
          body: HttpBody.text(createBody, "application/json"),
          headers: webhookHeaders(createBody, "installation"),
        })

        // Delete installation
        const deleteBody = JSON.stringify({
          action: "deleted",
          installation: {
            id: 33333,
            account: { type: "Organization", login: "del-org" },
          },
        })
        const res = yield* client.post("/webhooks/github", {
          body: HttpBody.text(deleteBody, "application/json"),
          headers: webhookHeaders(deleteBody, "installation"),
        })
        expect(res.status).toBe(200)

        // Verify installation row is gone
        const sql = yield* SqlClient.SqlClient
        const rows: readonly any[] = yield* sql.unsafe(
          `SELECT * FROM installations WHERE installation_id = 33333`,
        )
        expect(rows.length).toBe(0)
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )

  it.scopedLive(
    "upsertInstallation failure → 500, event row rolled back",
    () =>
      Effect.gen(function* () {
        yield* runDatabaseMigrations
        yield* router.pipe(HttpServer.serveEffect())
        const client = yield* HttpClient.HttpClient

        const body = JSON.stringify({
          action: "created",
          installation: {
            id: 77777,
            account: { type: "Organization", login: "fail-org" },
          },
          repositories: [{ full_name: "fail-org/repo1" }],
        })

        const res = yield* client.post("/webhooks/github", {
          body: HttpBody.text(body, "application/json"),
          headers: webhookHeaders(body, "installation"),
        })
        expect(res.status).toBe(500)
        const resBody = yield* res.json
        expect(resBody).toEqual({ error: "internal error" })

        // Verify event was rolled back — no row should exist
        const sql = yield* SqlClient.SqlClient
        const rows: readonly any[] = yield* sql.unsafe(
          `SELECT * FROM webhook_events WHERE installation_id = 77777`,
        )
        expect(rows.length).toBe(0)
      }).pipe(
        Effect.provide(
          makeServerTestLayer(
            DbLayer,
            Layer.effect(
              WebhookStore,
              Effect.gen(function* () {
                const sql = yield* SqlClient.SqlClient
                return {
                  insertEvent: (p) =>
                    sql`INSERT INTO webhook_events (delivery_id, event_type, action, repo_full_name, installation_id, payload)
                        VALUES (${p.deliveryId}, ${p.eventType}, ${p.action}, ${p.repoFullName}, ${p.installationId}, ${p.rawPayload})
                        RETURNING id`.pipe(Effect.map((rows) => rows.length > 0)),
                  upsertInstallation: () =>
                    Effect.fail(new SqlError.SqlError({ message: "injected failure" })),
                  getInstallationRepos: () => Effect.succeed(null),
                  deleteInstallation: () => Effect.void,
                  withTransaction: sql.withTransaction,
                }
              }),
            ).pipe(Layer.provide(DbLayer)),
          ),
        ),
      ),
    { timeout: 60_000 },
  )
})
