# CLI Authentication via SSH Signed Payload

## Overview

The CLI authenticates against the server using the developer's existing SSH key — no sign-up, no OAuth, no browser interaction. The CLI signs a payload containing its identity and a timestamp; the server verifies the signature against the user's GitHub-published keys and returns a short-lived JWT.

## Flow

```
  CLI                          Server                      GitHub API
   │                             │                              │
   │  1. Build payload:          │                              │
   │     { username,             │                              │
   │       fingerprint,          │                              │
   │       timestamp }           │                              │
   │     Sign payload with       │                              │
   │     private key             │                              │
   │     (ssh-keygen -Y sign)    │                              │
   │                             │                              │
   │  2. POST /auth/login        │                              │
   │     { username,             │                              │
   │       fingerprint,          │                              │
   │       timestamp,            │                              │
   │       signature }           │                              │
   │ ──────────────────────────► │                              │
   │                             │  3. Rate limit check (IP)    │
   │                             │     → 429 if exceeded        │
   │                             │                              │
   │                             │  4. Schema validation        │
   │                             │     → 400 if malformed       │
   │                             │                              │
   │                             │  5. Timestamp freshness      │
   │                             │     (60s window)             │
   │                             │     → 401 if stale           │
   │                             │                              │
   │                             │  6. Replay guard             │
   │                             │     (username, timestamp)    │
   │                             │     → 401 if duplicate       │
   │                             │                              │
   │                             │  7. GET /<user>.keys         │
   │                             │ ────────────────────────────►│
   │                             │                              │
   │                             │  8. Find key matching        │
   │                             │     fingerprint              │
   │                             │ ◄────────────────────────────│
   │                             │     → 403 if no match        │
   │                             │                              │
   │                             │  9. Verify SSH signature     │
   │                             │     (ssh-keygen -Y verify)   │
   │                             │     → 401 if invalid         │
   │                             │                              │
   │  10. { jwt (1h TTL) }      │                              │
   │ ◄────────────────────────── │                              │
   │                             │                              │
   │  CLI stores JWT in          │                              │
   │  ~/.config/brrr/session.json│                              │
   │                             │                              │
```

## Authenticated Requests

```
  CLI                          Server
   │                             │
   │  GET /events (SSE)          │
   │  Authorization: Bearer <jwt>│
   │ ──────────────────────────► │
   │                             │  Verify JWT signature + expiry
   │                             │  Check JTI against denylist
   │                             │  Extract claims, provide via Context
   │  Stream: event data...      │
   │ ◄────────────────────────── │
   │                             │
```

## Token Revocation

```
  CLI                          Server
   │                             │
   │  POST /auth/revoke          │
   │  Authorization: Bearer <jwt>│
   │  { "jti": "<token-id>" }   │
   │ ──────────────────────────► │
   │                             │  Add JTI to in-memory denylist
   │                             │  (TTL matches JWT max lifetime)
   │  { "revoked": true }       │
   │ ◄────────────────────────── │
   │                             │
```

## Token Refresh

Handled by `AuthClient`, which wraps every authenticated request with retry-once-on-401 logic:

```
  CLI (AuthClient)               Server
   │                               │
   │  Any request                  │
   │  Authorization: Bearer <jwt>  │
   │ ────────────────────────────► │
   │                               │
   │  401 Unauthorized             │
   │ ◄──────────────────────────── │
   │                               │
   │  Re-run SSH auth flow         │
   │  (automatic, no user input)   │
   │  ← obtains fresh JWT         │
   │                               │
   │  Retry original request       │
   │  Authorization: Bearer <new>  │
   │ ────────────────────────────► │
   │                               │
   │  200 OK (or error)            │
   │ ◄──────────────────────────── │
   │                               │
```

If the retry also returns 401, the error propagates to the caller (no infinite loop).

## JWT Claims

```json
{
  "sub": "SHA256:...",      // SSH public key fingerprint
  "ghuser": "username",     // GitHub username
  "jti": "uuid",            // Unique token ID (for revocation)
  "iat": 1709900000,
  "exp": 1709903600         // +1 hour
}
```

## Security Measures

- **Rate limiting** — Per-IP sliding window (5 req / 10s) on `/auth/login` prevents brute-force and GitHub API exhaustion
- **Replay guard** — Server-side `(username, timestamp)` dedup cache rejects replayed login requests within the freshness window
- **Schema validation** — Request bodies are decoded with `Effect.Schema`, preventing type confusion from unchecked casts
- **SSH key sanitization** — Control characters (newlines, etc.) in public key parts are rejected to prevent allowed_signers file injection
- **Token revocation** — JWTs carry a `jti` claim; `/auth/revoke` adds it to an in-memory denylist checked on every authenticated request
- **JWT secret enforcement** — `BRRR_JWT_SECRET` must be at least 32 bytes (256 bits); server refuses to start otherwise
- **Generic error messages** — Signature verification failures return a generic error without exposing internal failure reasons

## Endpoints

| Method | Path            | Auth     | Description                          |
|--------|-----------------|----------|--------------------------------------|
| GET    | /health         | None     | Health check                         |
| POST   | /auth/login     | None     | SSH-signed login, returns JWT        |
| POST   | /auth/revoke    | Bearer   | Revoke a token by JTI                |
| GET    | /whoami         | Bearer   | Returns authenticated user's claims  |

## Why This Works

- **No sign-up** — uses the SSH key the developer already has
- **No browser** — single request, fully non-interactive
- **Single round trip** — CLI signs locally, server verifies in one request
- **Verifiable identity** — GitHub's public API confirms the pubkey belongs to a real user
- **Secure** — private key never leaves the machine; only a signature over a self-describing payload is transmitted
- **Replay-resistant** — timestamp freshness + server-side dedup guard
- **Revocable** — JTI-based denylist allows early token invalidation
- **Auto-refresh** — CLI re-authenticates transparently on 401
