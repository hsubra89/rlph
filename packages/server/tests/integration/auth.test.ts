import {
  HttpBody,
  HttpClient,
  HttpRouter,
  HttpServer,
  HttpServerResponse,
} from "@effect/platform"
import { NodeCommandExecutor, NodeFileSystem, NodeHttpServer } from "@effect/platform-node"
import { describe, expect, it } from "@effect/vitest"
import { Effect, Layer } from "effect"
import * as crypto from "node:crypto"
import * as jose from "jose"
import { makeHandleLogin } from "../../src/auth/login.js"
import { LoginRateLimiterLive } from "../../src/auth/login-rate-limiter.js"
import { makeAuthMiddleware } from "../../src/auth/middleware.js"
import { ReplayGuardLive } from "../../src/auth/replay-guard.js"
import { handleRevoke } from "../../src/auth/revoke.js"
import { TokenDenylistLive } from "../../src/auth/token-denylist.js"
import { handleWhoami } from "../../src/auth/whoami.js"

const JWT_SECRET = new TextEncoder().encode("test-secret")

function makeRouter() {
  const authMiddleware = makeAuthMiddleware(JWT_SECRET)
  return HttpRouter.empty.pipe(
    HttpRouter.get("/health", HttpServerResponse.json({ status: "ok" })),
    HttpRouter.post("/auth/login", makeHandleLogin(JWT_SECRET)),
    HttpRouter.get("/whoami", authMiddleware(handleWhoami)),
    HttpRouter.post("/auth/revoke", authMiddleware(handleRevoke)),
  )
}

const TestLayer = Layer.mergeAll(
  NodeHttpServer.layerTest,
  NodeCommandExecutor.layer.pipe(Layer.provideMerge(NodeFileSystem.layer)),
  ReplayGuardLive,
  TokenDenylistLive,
  LoginRateLimiterLive,
)

function mintJwt(opts: { ghuser: string; sub: string; jti?: string }) {
  return new jose.SignJWT({ ghuser: opts.ghuser })
    .setProtectedHeader({ alg: "HS256" })
    .setSubject(opts.sub)
    .setJti(opts.jti ?? crypto.randomUUID())
    .setIssuedAt()
    .setExpirationTime("1h")
    .sign(JWT_SECRET)
}

describe("auth flow", () => {
  it.scoped("GET /health returns 200", () =>
    Effect.gen(function* () {
      yield* makeRouter().pipe(HttpServer.serveEffect())
      const client = yield* HttpClient.HttpClient
      const res = yield* client.get("/health")
      expect(res.status).toBe(200)
      const body = yield* res.json
      expect(body).toEqual({ status: "ok" })
    }).pipe(Effect.provide(TestLayer)),
  )

  it.scoped("POST /auth/login rejects invalid body with 400", () =>
    Effect.gen(function* () {
      yield* makeRouter().pipe(HttpServer.serveEffect())
      const client = yield* HttpClient.HttpClient
      const res = yield* client.post("/auth/login", {
        body: HttpBody.unsafeJson({ bad: "data" }),
      })
      expect(res.status).toBe(400)
      const body = yield* res.json
      expect(body).toEqual({ error: "username, fingerprint, timestamp, and signature required" })
    }).pipe(Effect.provide(TestLayer)),
  )

  it.scoped("POST /auth/login rejects string timestamp with 400", () =>
    Effect.gen(function* () {
      yield* makeRouter().pipe(HttpServer.serveEffect())
      const client = yield* HttpClient.HttpClient
      const res = yield* client.post("/auth/login", {
        body: HttpBody.unsafeJson({
          username: "testuser",
          fingerprint: "SHA256:abc",
          timestamp: "0",
          signature: "fake",
        }),
      })
      expect(res.status).toBe(400)
    }).pipe(Effect.provide(TestLayer)),
  )

  it.scoped("POST /auth/login rejects stale timestamp with 401", () =>
    Effect.gen(function* () {
      yield* makeRouter().pipe(HttpServer.serveEffect())
      const client = yield* HttpClient.HttpClient
      const res = yield* client.post("/auth/login", {
        body: HttpBody.unsafeJson({
          username: "testuser",
          fingerprint: "SHA256:abc",
          timestamp: 1000000,
          signature: "fake",
        }),
      })
      expect(res.status).toBe(401)
      const body = yield* res.json
      expect(body).toEqual({ error: "timestamp too stale" })
    }).pipe(Effect.provide(TestLayer)),
  )

  it.scoped("POST /auth/login rejects replayed request with 401", () =>
    Effect.gen(function* () {
      yield* makeRouter().pipe(HttpServer.serveEffect())
      const client = yield* HttpClient.HttpClient
      const timestamp = Math.floor(Date.now() / 1000)
      const loginBody = HttpBody.unsafeJson({
        username: "replayuser",
        fingerprint: "SHA256:abc",
        timestamp,
        signature: "fake",
      })

      // First request — passes replay guard, fails at GitHub fetch (403)
      const res1 = yield* client.post("/auth/login", { body: loginBody })
      expect(res1.status).toBe(403)

      // Second request — same payload, rejected by replay guard
      const res2 = yield* client.post("/auth/login", { body: loginBody })
      expect(res2.status).toBe(401)
      const body = yield* res2.json
      expect(body).toEqual({ error: "duplicate request" })
    }).pipe(Effect.provide(TestLayer)),
  )

  it.scoped("POST /auth/login returns 429 after too many requests", () =>
    Effect.gen(function* () {
      yield* makeRouter().pipe(HttpServer.serveEffect())
      const client = yield* HttpClient.HttpClient

      // Send 5 requests (the limit) — all should get through (fail at GitHub, 403)
      for (let i = 0; i < 5; i++) {
        const res = yield* client.post("/auth/login", {
          body: HttpBody.unsafeJson({
            username: `ratelimituser${i}`,
            fingerprint: "SHA256:abc",
            timestamp: Math.floor(Date.now() / 1000),
            signature: "fake",
          }),
        })
        expect(res.status).not.toBe(429)
      }

      // 6th request should be rate-limited
      const res = yield* client.post("/auth/login", {
        body: HttpBody.unsafeJson({
          username: "ratelimituser-extra",
          fingerprint: "SHA256:abc",
          timestamp: Math.floor(Date.now() / 1000),
          signature: "fake",
        }),
      })
      expect(res.status).toBe(429)
      const body = yield* res.json
      expect(body).toEqual({ error: "too many requests" })
    }).pipe(Effect.provide(TestLayer)),
  )

  it.scoped("GET /whoami rejects missing auth header with 401", () =>
    Effect.gen(function* () {
      yield* makeRouter().pipe(HttpServer.serveEffect())
      const client = yield* HttpClient.HttpClient
      const res = yield* client.get("/whoami")
      expect(res.status).toBe(401)
      const body = yield* res.json
      expect(body).toEqual({ error: "Missing or invalid Authorization header" })
    }).pipe(Effect.provide(TestLayer)),
  )

  it.scoped("GET /whoami rejects invalid token with 401", () =>
    Effect.gen(function* () {
      yield* makeRouter().pipe(HttpServer.serveEffect())
      const client = yield* HttpClient.HttpClient
      const res = yield* client.get("/whoami", {
        headers: { authorization: "Bearer not.a.real.token" },
      })
      expect(res.status).toBe(401)
      const body = yield* res.json
      expect(body).toEqual({ error: "Invalid or expired token" })
    }).pipe(Effect.provide(TestLayer)),
  )

  it.scoped("GET /whoami succeeds with valid token", () =>
    Effect.gen(function* () {
      yield* makeRouter().pipe(HttpServer.serveEffect())
      const client = yield* HttpClient.HttpClient
      const token = yield* Effect.promise(() =>
        mintJwt({ ghuser: "alice", sub: "SHA256:fp" }),
      )
      const res = yield* client.get("/whoami", {
        headers: { authorization: `Bearer ${token}` },
      })
      expect(res.status).toBe(200)
      const body = yield* res.json
      expect(body).toEqual({ ghuser: "alice", sub: "SHA256:fp" })
    }).pipe(Effect.provide(TestLayer)),
  )

  it.scoped("POST /auth/revoke invalidates token", () =>
    Effect.gen(function* () {
      yield* makeRouter().pipe(HttpServer.serveEffect())
      const client = yield* HttpClient.HttpClient

      const jti = crypto.randomUUID()
      const token = yield* Effect.promise(() =>
        mintJwt({ ghuser: "bob", sub: "SHA256:fp2", jti }),
      )
      const auth = { authorization: `Bearer ${token}` }

      // Token works before revocation
      const res1 = yield* client.get("/whoami", { headers: auth })
      expect(res1.status).toBe(200)

      // Revoke the token
      const revokeRes = yield* client.post("/auth/revoke", {
        headers: auth,
        body: HttpBody.unsafeJson({ jti }),
      })
      expect(revokeRes.status).toBe(200)
      const revokeBody = yield* revokeRes.json
      expect(revokeBody).toEqual({ revoked: true })

      // Token is now rejected
      const res2 = yield* client.get("/whoami", { headers: auth })
      expect(res2.status).toBe(401)
      const body = yield* res2.json
      expect(body).toEqual({ error: "Token has been revoked" })
    }).pipe(Effect.provide(TestLayer)),
  )

  it.scoped("POST /auth/revoke rejects missing jti with 400", () =>
    Effect.gen(function* () {
      yield* makeRouter().pipe(HttpServer.serveEffect())
      const client = yield* HttpClient.HttpClient
      const token = yield* Effect.promise(() =>
        mintJwt({ ghuser: "carol", sub: "SHA256:fp3" }),
      )
      const res = yield* client.post("/auth/revoke", {
        headers: { authorization: `Bearer ${token}` },
        body: HttpBody.unsafeJson({}),
      })
      expect(res.status).toBe(400)
      const body = yield* res.json
      expect(body).toEqual({ error: "jti is required" })
    }).pipe(Effect.provide(TestLayer)),
  )
})
