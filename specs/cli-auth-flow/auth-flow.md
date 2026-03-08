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
   │                             │  3. GET /<user>.keys         │
   │                             │ ────────────────────────────►│
   │                             │                              │
   │                             │  4. Find key matching        │
   │                             │     fingerprint              │
   │                             │ ◄────────────────────────────│
   │                             │                              │
   │                             │  5. Verify:                  │
   │                             │     - timestamp within 60s   │
   │                             │     - signature valid for    │
   │                             │       matched public key     │
   │                             │                              │
   │  6. { jwt (1h TTL) }       │                              │
   │ ◄────────────────────────── │                              │
   │                             │                              │
   │  CLI stores JWT in          │                              │
   │  ~/.config/rlph/session.json│                              │
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
   │                             │  Extract user identity from sub claim
   │  Stream: event data...      │
   │ ◄────────────────────────── │
   │                             │
```

## Token Refresh

```
  CLI                          Server
   │                             │
   │  Any request                │
   │  Authorization: Bearer <jwt>│
   │ ──────────────────────────► │
   │                             │
   │  401 Unauthorized           │
   │ ◄────────────────────────── │
   │                             │
   │  Re-run auth flow           │
   │  (automatic, no user input) │
   │ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ─ ► │
   │                             │
```

## JWT Claims

```json
{
  "sub": "SHA256:...",      // SSH public key fingerprint
  "ghuser": "username",     // GitHub username
  "iat": 1709900000,
  "exp": 1709903600         // +1 hour
}
```

## Why This Works

- **No sign-up** — uses the SSH key the developer already has
- **No browser** — single request, fully non-interactive
- **Single round trip** — CLI signs locally, server verifies in one request
- **Verifiable identity** — GitHub's public API confirms the pubkey belongs to a real user
- **Secure** — private key never leaves the machine; only a signature over a self-describing payload is transmitted
- **Replay-resistant** — timestamp freshness window (60s) prevents reuse of captured signatures
- **Stateless sessions** — JWT requires no server-side storage
- **Auto-refresh** — CLI re-authenticates transparently on 401
