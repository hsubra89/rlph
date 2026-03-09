# packages/server (@brrr/server)

Auth API server. Handles SSH challenge-response login, JWT token lifecycle, rate limiting, and replay protection.

## Stack

- **Effect-TS** — functional composition, services via `Context.Tag`, `Layer` for DI
- **@effect/platform** + **@effect/platform-node** — HTTP server, filesystem, command execution
- **jose** — JWT signing/verification
- **vitest** + **@effect/vitest** — testing

## Architecture

Entry point (`main.ts`) runs Postgres migrations, then composes layers:

```
runDatabaseMigrations
  → ServerLive → NodeContext → ReplayGuardLive → TokenDenylistLive
  → LoginRateLimiterLive → DatabaseHealthLive → FetchHttpClient → AppConfigLiveLayer
```

### Routes (`router.ts`)

| Method | Path | Auth | Purpose |
|--------|------|------|---------|
| GET | `/health` | No | Readiness check backed by Postgres connectivity |
| POST | `/auth/login` | No | SSH challenge-response → JWT |
| GET | `/whoami` | Yes | Authenticated user info |
| POST | `/auth/revoke` | Yes | Token revocation |

### Auth Flow (`auth/`)

| Module | Purpose |
|--------|---------|
| `login.ts` | Verify GitHub SSH keys against signature, issue JWT. Rate limiting + replay prevention + timestamp freshness. |
| `middleware.ts` | JWT verification middleware |
| `ssh.ts` | SSH signature verification, fingerprint extraction |
| `whoami.ts` | Return authenticated user info |
| `revoke.ts` | Token revocation |
| `replay-guard.ts` | Block replayed (username, timestamp) pairs |
| `login-rate-limiter.ts` | Per-IP rate limiting |
| `token-denylist.ts` | Revoked token tracking |
| `database.ts` | Required Postgres client, health service, and migration runner |
| `health.ts` | Readiness endpoint backed by `DatabaseHealth` |
| `constants.ts` | `JWT_EXPIRY`, `TIMESTAMP_FRESHNESS_SECS` |
| `map-utils.ts` | Effect Map utility functions |

### Config (`config.ts`)

`AppConfig` context:

- port (env `BRRR_PORT`, default 3000)
- JWT secret (env `BRRR_JWT_SECRET`, must be >= 32 bytes)
- Postgres URL (env `BRRR_POSTGRES_URL`, required)

## Effect Patterns

- Services defined with `Context.Tag`, provided via `Layer`
- Use `Effect.gen` for sequential effectful code
- Errors are typed — use `Effect.fail` with tagged error types
- Layer composition: `Layer.provide` / `Layer.provideMerge`

## Commands

- **Dev:** `pnpm dev` (tsx)
- **Build:** `pnpm build` (tsc → dist/)
- **Start:** `pnpm start` (node dist/)
- **Test (unit):** `pnpm test`
- **Test (integration):** `pnpm test:integration`
- **Test (all):** `pnpm test:all`
- **Lint:** `pnpm lint` (oxlint)
- **Format:** `pnpm fmt` (oxfmt) / `pnpm fmt:check`
