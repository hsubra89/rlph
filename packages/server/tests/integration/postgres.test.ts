import { HttpClient, HttpServer } from "@effect/platform"
import { describe, expect, it } from "@effect/vitest"
import { Effect } from "effect"
import { runDatabaseMigrations } from "../../src/database.js"
import { router } from "../../src/router.js"
import { makeServerTestLayer, PgContainer } from "./fixtures.js"

const TestLayer = makeServerTestLayer(PgContainer.TestDatabaseLayer)

describe("postgres foundation", () => {
  it.scopedLive(
    "applies the baseline migration",
    () =>
      Effect.gen(function* () {
        const applied = yield* runDatabaseMigrations
        expect(applied).toEqual([
          [1, "postgres_foundation"],
          [2, "webhook_tables"],
          [3, "webhook_delivery_id"],
        ])
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )

  it.scopedLive(
    "GET /health returns 200 while the server is running",
    () =>
      Effect.gen(function* () {
        yield* runDatabaseMigrations
        yield* router.pipe(HttpServer.serveEffect())
        const client = yield* HttpClient.HttpClient
        const res = yield* client.get("/health")
        expect(res.status).toBe(200)
        const body = yield* res.json
        expect(body).toEqual({ status: "ok" })
      }).pipe(Effect.provide(TestLayer)),
    { timeout: 60_000 },
  )
})
