# CLI Authentication via SSH Challenge-Response

## Overview

The CLI authenticates against the server using the developer's existing SSH key — no sign-up, no OAuth, no browser interaction. The server issues a short-lived JWT after verifying the user holds the private key.

## Flow

```
  CLI                          Server                      GitHub API
   │                             │                              │
   │  1. POST /auth/challenge    │                              │
   │     { pubkey, username }    │                              │
   │ ──────────────────────────► │                              │
   │                             │  2. GET /<user>.keys         │
   │                             │ ────────────────────────────►│
   │                             │                              │
   │                             │  3. Verify pubkey belongs    │
   │                             │     to this GitHub user      │
   │                             │ ◄────────────────────────────│
   │                             │                              │
   │  4. { nonce }               │                              │
   │ ◄────────────────────────── │                              │
   │                             │                              │
   │  5. Sign nonce with         │                              │
   │     private key             │                              │
   │     (ssh-keygen -Y sign)    │                              │
   │                             │                              │
   │  6. POST /auth/verify       │                              │
   │     { pubkey, signature }   │                              │
   │ ──────────────────────────► │                              │
   │                             │                              │
   │                             │  7. Verify signature         │
   │                             │     against pubkey           │
   │                             │                              │
   │  8. { jwt (1h TTL) }       │                              │
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
   │  Re-run challenge-response  │
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
- **No browser** — challenge-response is fully non-interactive
- **Verifiable identity** — GitHub's public API confirms the pubkey belongs to a real user
- **Secure** — private key never leaves the machine; only a signature over a server-generated nonce is transmitted
- **Stateless sessions** — JWT requires no server-side storage
- **Auto-refresh** — CLI re-authenticates transparently on 401
