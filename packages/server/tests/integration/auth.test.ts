import { HttpBody, HttpClient, HttpServer } from "@effect/platform";
import { NodeCommandExecutor, NodeFileSystem, NodeHttpServer } from "@effect/platform-node";
import { describe, expect, it } from "@effect/vitest";
import { Effect, Layer } from "effect";
import * as crypto from "node:crypto";
import * as jose from "jose";
import { JWT_EXPIRY, unixNowSecs } from "../../src/auth/constants.js";
import { LoginRateLimiterLive } from "../../src/auth/login-rate-limiter.js";
import { ReplayGuardLive } from "../../src/auth/replay-guard.js";
import { TokenDenylistLive } from "../../src/auth/token-denylist.js";
import { AppConfig, AppConfigTag } from "../../src/config.js";
import { router } from "../../src/router.js";

const JWT_SECRET = new TextEncoder().encode("test-secret-that-is-at-least-32-bytes-long");

const TestConfigLayer = Layer.succeed(AppConfigTag, new AppConfig(0, JWT_SECRET));

const TestLayer = Layer.mergeAll(
  NodeHttpServer.layerTest,
  NodeCommandExecutor.layer.pipe(Layer.provideMerge(NodeFileSystem.layer)),
  ReplayGuardLive,
  TokenDenylistLive,
  LoginRateLimiterLive,
  TestConfigLayer,
);

function mintJwt(opts: { ghuser: string; sub: string; jti?: string }) {
  return new jose.SignJWT({ ghuser: opts.ghuser })
    .setProtectedHeader({ alg: "HS256" })
    .setSubject(opts.sub)
    .setJti(opts.jti ?? crypto.randomUUID())
    .setIssuedAt()
    .setExpirationTime(JWT_EXPIRY)
    .sign(JWT_SECRET);
}

describe("auth flow", () => {
  it.scoped("GET /health returns 200", () =>
    Effect.gen(function* () {
      yield* router.pipe(HttpServer.serveEffect());
      const client = yield* HttpClient.HttpClient;
      const res = yield* client.get("/health");
      expect(res.status).toBe(200);
      const body = yield* res.json;
      expect(body).toEqual({ status: "ok" });
    }).pipe(Effect.provide(TestLayer)),
  );

  it.scoped("POST /auth/login rejects invalid body with 400", () =>
    Effect.gen(function* () {
      yield* router.pipe(HttpServer.serveEffect());
      const client = yield* HttpClient.HttpClient;
      const res = yield* client.post("/auth/login", {
        body: HttpBody.unsafeJson({ bad: "data" }),
      });
      expect(res.status).toBe(400);
      const body = yield* res.json;
      expect(body).toEqual({ error: "username, fingerprint, timestamp, and signature required" });
    }).pipe(Effect.provide(TestLayer)),
  );

  it.scoped("POST /auth/login rejects string timestamp with 400", () =>
    Effect.gen(function* () {
      yield* router.pipe(HttpServer.serveEffect());
      const client = yield* HttpClient.HttpClient;
      const res = yield* client.post("/auth/login", {
        body: HttpBody.unsafeJson({
          username: "testuser",
          fingerprint: "SHA256:abc",
          timestamp: "0",
          signature: "fake",
        }),
      });
      expect(res.status).toBe(400);
    }).pipe(Effect.provide(TestLayer)),
  );

  it.scoped("POST /auth/login rejects stale timestamp with 401", () =>
    Effect.gen(function* () {
      yield* router.pipe(HttpServer.serveEffect());
      const client = yield* HttpClient.HttpClient;
      const res = yield* client.post("/auth/login", {
        body: HttpBody.unsafeJson({
          username: "testuser",
          fingerprint: "SHA256:abc",
          timestamp: 1000000,
          signature: "fake",
        }),
      });
      expect(res.status).toBe(401);
      const body = yield* res.json;
      expect(body).toEqual({ error: "timestamp too stale" });
    }).pipe(Effect.provide(TestLayer)),
  );

  it.scoped("POST /auth/login rejects replayed request with 401", () =>
    Effect.gen(function* () {
      yield* router.pipe(HttpServer.serveEffect());
      const client = yield* HttpClient.HttpClient;
      const timestamp = unixNowSecs();
      const loginBody = HttpBody.unsafeJson({
        username: "replayuser",
        fingerprint: "SHA256:abc",
        timestamp,
        signature: "fake",
      });

      // First request — passes replay guard, fails at GitHub fetch (502 in test env)
      const res1 = yield* client.post("/auth/login", { body: loginBody });
      expect(res1.status).toBe(502);

      // Second request — same payload, rejected by replay guard
      const res2 = yield* client.post("/auth/login", { body: loginBody });
      expect(res2.status).toBe(401);
      const body = yield* res2.json;
      expect(body).toEqual({ error: "duplicate request" });
    }).pipe(Effect.provide(TestLayer)),
  );

  it.scoped("POST /auth/login returns 429 after too many requests", () =>
    Effect.gen(function* () {
      yield* router.pipe(HttpServer.serveEffect());
      const client = yield* HttpClient.HttpClient;

      // Send 5 requests (the limit) — all should get through (fail at GitHub, 403)
      for (let i = 0; i < 5; i++) {
        const res = yield* client.post("/auth/login", {
          body: HttpBody.unsafeJson({
            username: `ratelimituser${i}`,
            fingerprint: "SHA256:abc",
            timestamp: unixNowSecs(),
            signature: "fake",
          }),
        });
        expect(res.status).not.toBe(429);
      }

      // 6th request should be rate-limited
      const res = yield* client.post("/auth/login", {
        body: HttpBody.unsafeJson({
          username: "ratelimituser-extra",
          fingerprint: "SHA256:abc",
          timestamp: unixNowSecs(),
          signature: "fake",
        }),
      });
      expect(res.status).toBe(429);
      const body = yield* res.json;
      expect(body).toEqual({ error: "too many requests" });
    }).pipe(Effect.provide(TestLayer)),
  );

  it.scoped("GET /whoami rejects missing auth header with 401", () =>
    Effect.gen(function* () {
      yield* router.pipe(HttpServer.serveEffect());
      const client = yield* HttpClient.HttpClient;
      const res = yield* client.get("/whoami");
      expect(res.status).toBe(401);
      const body = yield* res.json;
      expect(body).toEqual({ error: "missing or invalid authorization header" });
    }).pipe(Effect.provide(TestLayer)),
  );

  it.scoped("GET /whoami rejects invalid token with 401", () =>
    Effect.gen(function* () {
      yield* router.pipe(HttpServer.serveEffect());
      const client = yield* HttpClient.HttpClient;
      const res = yield* client.get("/whoami", {
        headers: { authorization: "Bearer not.a.real.token" },
      });
      expect(res.status).toBe(401);
      const body = yield* res.json;
      expect(body).toEqual({ error: "invalid or expired token" });
    }).pipe(Effect.provide(TestLayer)),
  );

  it.scoped("GET /whoami succeeds with valid token", () =>
    Effect.gen(function* () {
      yield* router.pipe(HttpServer.serveEffect());
      const client = yield* HttpClient.HttpClient;
      const token = yield* Effect.promise(() => mintJwt({ ghuser: "alice", sub: "SHA256:fp" }));
      const res = yield* client.get("/whoami", {
        headers: { authorization: `Bearer ${token}` },
      });
      expect(res.status).toBe(200);
      const body = yield* res.json;
      expect(body).toEqual({ ghuser: "alice", sub: "SHA256:fp" });
    }).pipe(Effect.provide(TestLayer)),
  );

  it.scoped("POST /auth/revoke invalidates token", () =>
    Effect.gen(function* () {
      yield* router.pipe(HttpServer.serveEffect());
      const client = yield* HttpClient.HttpClient;

      const jti = crypto.randomUUID();
      const token = yield* Effect.promise(() => mintJwt({ ghuser: "bob", sub: "SHA256:fp2", jti }));
      const auth = { authorization: `Bearer ${token}` };

      // Token works before revocation
      const res1 = yield* client.get("/whoami", { headers: auth });
      expect(res1.status).toBe(200);

      // Revoke the token (no body needed — endpoint uses caller's own JTI)
      const revokeRes = yield* client.post("/auth/revoke", {
        headers: auth,
      });
      expect(revokeRes.status).toBe(200);
      const revokeBody = yield* revokeRes.json;
      expect(revokeBody).toEqual({ revoked: true });

      // Token is now rejected
      const res2 = yield* client.get("/whoami", { headers: auth });
      expect(res2.status).toBe(401);
      const body = yield* res2.json;
      expect(body).toEqual({ error: "token has been revoked" });
    }).pipe(Effect.provide(TestLayer)),
  );

  it.scoped("POST /auth/revoke only revokes caller's own token", () =>
    Effect.gen(function* () {
      yield* router.pipe(HttpServer.serveEffect());
      const client = yield* HttpClient.HttpClient;

      const jti1 = crypto.randomUUID();
      const jti2 = crypto.randomUUID();
      const token1 = yield* Effect.promise(() => mintJwt({ ghuser: "carol", sub: "SHA256:fp3", jti: jti1 }));
      const token2 = yield* Effect.promise(() => mintJwt({ ghuser: "dave", sub: "SHA256:fp4", jti: jti2 }));

      // Revoke token1 — send jti2 in body to confirm the body is ignored
      const revokeRes = yield* client.post("/auth/revoke", {
        headers: { authorization: `Bearer ${token1}` },
        body: HttpBody.unsafeJson({ jti: jti2 }),
      });
      expect(revokeRes.status).toBe(200);

      // token1 is revoked
      const res1 = yield* client.get("/whoami", {
        headers: { authorization: `Bearer ${token1}` },
      });
      expect(res1.status).toBe(401);

      // token2 is unaffected
      const res2 = yield* client.get("/whoami", {
        headers: { authorization: `Bearer ${token2}` },
      });
      expect(res2.status).toBe(200);
    }).pipe(Effect.provide(TestLayer)),
  );
});
