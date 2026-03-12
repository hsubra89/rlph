# GitHub Webhook Receiver

## Overview

The server receives webhook events from a GitHub App and stores them as raw JSONB in Postgres. Events are not processed immediately — the CLI polls for new events later. Authentication is via HMAC-SHA256 signature verification (standard GitHub webhook secret).

## Flow

```
  GitHub                         Server                       Postgres
   │                               │                             │
   │  1. POST /webhooks/github     │                             │
   │     X-Hub-Signature-256:      │                             │
   │       sha256=<hmac>           │                             │
   │     X-GitHub-Event: push      │                             │
   │     X-GitHub-Delivery: <uuid> │                             │
   │     Body: { ... }             │                             │
   │ ────────────────────────────► │                             │
   │                               │  2. Read raw body as text   │
   │                               │     (before JSON parsing)   │
   │                               │                             │
   │                               │  3. Verify HMAC-SHA256      │
   │                               │     → 401 if missing/invalid│
   │                               │                             │
   │                               │  4. Parse JSON, extract:    │
   │                               │     event_type (header)     │
   │                               │     action (payload)        │
   │                               │     repo (payload)          │
   │                               │     installation_id         │
   │                               │                             │
   │                               │  5. INSERT webhook_events   │
   │                               │ ────────────────────────────►
   │                               │                             │
   │  6. 200 { received: true }    │                             │
   │ ◄──────────────────────────── │                             │
   │                               │                             │
```

## Subscribed Events

| Category | Events |
|----------|--------|
| PR lifecycle | `pull_request`, `pull_request_review`, `pull_request_review_comment` |
| Issues & comments | `issues`, `issue_comment` |
| Push & CI | `push`, `check_run`, `check_suite`, `status` |
| Installations | `installation`, `installation_repositories` |

All events are stored in the raw event log. Installation events additionally trigger upserts to the `installations` table (see [installations spec](2-installations.md)).

## Endpoint

| Method | Path | Auth | Description |
|--------|------|------|-------------|
| POST | `/webhooks/github` | HMAC | Receive and store GitHub webhook events |

No JWT auth middleware — HMAC signature is the authentication mechanism.

## HMAC Signature Verification

GitHub sends `X-Hub-Signature-256: sha256=<hex>` on every webhook delivery.

Verification steps:
1. Extract `X-Hub-Signature-256` header → 401 `{ "error": "missing signature" }` if absent
2. Compute `sha256=` + HMAC-SHA256(webhook_secret, raw_body_text).hexdigest
3. Constant-time compare via `crypto.timingSafeEqual` → 401 `{ "error": "invalid signature" }` if mismatch

The raw body must be read as text before any JSON parsing to ensure byte-exact HMAC input.

## Database Schema

```sql
CREATE TABLE webhook_events (
  id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  event_type      TEXT NOT NULL,          -- X-GitHub-Event header value
  action          TEXT,                   -- payload.action (nullable)
  repo_full_name  TEXT,                   -- payload.repository.full_name (nullable)
  installation_id BIGINT,                 -- payload.installation.id (nullable)
  payload         JSONB NOT NULL,         -- full raw payload
  received_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_webhook_events_type_action ON webhook_events (event_type, action);
CREATE INDEX idx_webhook_events_repo ON webhook_events (repo_full_name);
CREATE INDEX idx_webhook_events_received ON webhook_events (received_at);
```

Append-only log. No updates or deletes. The CLI queries by `event_type`, `repo_full_name`, and `received_at` when polling for work.

## Handler Logic

```
handleWebhook = Effect.gen:
  1. request.text           → rawBody (string)
  2. request.headers        → sig = X-Hub-Signature-256
                             → eventType = X-GitHub-Event
                             → deliveryId = X-GitHub-Delivery
  3. if !sig                → 401 { error: "missing signature" }
  4. verifyWebhookSignature(secret, sig, rawBody)
     on failure             → 401 { error: "invalid signature" }
  5. if !eventType          → 400 { error: "missing event type" }
  6. JSON.parse(rawBody)    → payload
  7. extract action, repo_full_name, installation_id from payload
  8. WebhookStore.insertEvent(...)
  9. if eventType ∈ {installation, installation_repositories}
       → WebhookStore.upsertInstallation(...)
  10. 200 { received: true }
```

## Error Responses

All errors follow the convention: lowercase, no punctuation.

| Status | Body | Condition |
|--------|------|-----------|
| 401 | `{ "error": "missing signature" }` | No `X-Hub-Signature-256` header |
| 401 | `{ "error": "invalid signature" }` | HMAC mismatch |
| 400 | `{ "error": "missing event type" }` | No `X-GitHub-Event` header |
| 500 | `{ "error": "internal error" }` | DB insert failure (logged, not exposed) |

## Configuration

| Env var | Required | Description |
|---------|----------|-------------|
| `BRRR_GITHUB_WEBHOOK_SECRET` | Yes | Shared secret configured in the GitHub App's webhook settings |

Provided as `GitHubWebhookSecret` Context.Tag service via `Config.redacted`.

## Effect Services

| Service | Tag | Layer | Dependencies |
|---------|-----|-------|--------------|
| `GitHubWebhookSecret` | `"GitHubWebhookSecret"` | `GitHubWebhookSecretLive` | Config |
| `WebhookStore` | `"WebhookStore"` | `WebhookStoreLive` | `SqlClient.SqlClient` |

## New Files

| File | Purpose |
|------|---------|
| `src/github/config.ts` | `GitHubWebhookSecret` tag + live layer |
| `src/github/webhook-signature.ts` | Pure `verifyWebhookSignature` function |
| `src/github/webhook-store.ts` | `WebhookStore` service (insertEvent, upsertInstallation) |
| `src/github/webhook-handler.ts` | Route handler |
| `src/migrations/0002_webhook_tables.ts` | Migration for both tables |

## Modified Files

| File | Change |
|------|--------|
| `src/router.ts` | Add `HttpRouter.post("/webhooks/github", handleWebhook)` |
| `src/main.ts` | Provide `GitHubWebhookSecretLive`, `WebhookStoreLive`, `PostgresLive` layers |
| `src/database.ts` | Register `0002_webhook_tables` in `PgMigrator.fromRecord` |

## Testing

**Unit** (`tests/unit/webhook-signature.test.ts`):
- Valid HMAC passes
- Tampered body fails
- Wrong secret fails
- Missing `sha256=` prefix fails

**Integration** (`tests/integration/webhook.test.ts`):
- Valid webhook stores event in DB (query to confirm)
- Invalid signature → 401
- Missing signature header → 401
- Missing event type header → 400
- Verify `received_at` is populated
- Verify payload JSONB is queryable
