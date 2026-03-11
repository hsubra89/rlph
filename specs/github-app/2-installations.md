# GitHub App Installations

## Overview

Track which orgs/users have installed the GitHub App and which repos are accessible. Multi-tenant — the server handles installations across multiple GitHub accounts. Installation records are persisted to Postgres and updated reactively via webhook events.

## How Installations Are Tracked

The server does not poll GitHub for installation state. Instead, it reacts to two webhook event types:

| Event | Action | Effect |
|-------|--------|--------|
| `installation` | `created` | Upsert installation record |
| `installation` | `deleted` | Could delete or mark inactive (v1: upsert with empty repos) |
| `installation` | `suspend` / `unsuspend` | Upsert (future: track suspension state) |
| `installation_repositories` | `added` / `removed` | Update repos list |

These events are received by the webhook handler (see [webhook spec](1-webhook-receiver.md)) and routed to `WebhookStore.upsertInstallation`.

## Database Schema

```sql
CREATE TABLE installations (
  installation_id BIGINT PRIMARY KEY,     -- GitHub's installation ID
  account_type    TEXT NOT NULL,           -- 'Organization' or 'User'
  account_login   TEXT NOT NULL,           -- org or user login name
  repos           JSONB,                   -- array of repo full_names, null = all repos
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Created in the same migration as `webhook_events` (`0002_webhook_tables`).

### `repos` Column

- `null` — App installed on all repos in the account
- `["org/repo-a", "org/repo-b"]` — App installed on specific repos

When `installation_repositories` events arrive with `added`/`removed` actions, the repos array is replaced wholesale from the webhook payload's `repositories` field (not incrementally patched).

## Upsert Logic

```
upsertInstallation({ installation_id, account_type, account_login, repos }):
  INSERT INTO installations (installation_id, account_type, account_login, repos)
  VALUES ($1, $2, $3, $4)
  ON CONFLICT (installation_id) DO UPDATE SET
    account_type = EXCLUDED.account_type,
    account_login = EXCLUDED.account_login,
    repos = EXCLUDED.repos,
    updated_at = now()
```

## Payload Extraction

From `installation` events:
```
installation_id  = payload.installation.id
account_type     = payload.installation.account.type      -- "Organization" | "User"
account_login    = payload.installation.account.login
repos            = payload.repositories                    -- null if "all repos" selection
```

From `installation_repositories` events:
```
installation_id  = payload.installation.id
account_type     = payload.installation.account.type
account_login    = payload.installation.account.login
repos            = payload.repositories (full list after add/remove)
```

Note: `installation_repositories` events include `repositories_added` and `repositories_removed` arrays, but we rebuild the full list from `payload.repositories` for simplicity.

## Querying Installations

The CLI (or future server features) can query:

```sql
-- All installations for an org
SELECT * FROM installations WHERE account_login = 'my-org';

-- Check if a repo is covered
SELECT * FROM installations
WHERE repos IS NULL  -- all repos
   OR repos @> '"org/repo-name"';
```

## Testing

**Integration** (in `tests/integration/webhook.test.ts`):
- `installation` event with `action: "created"` → row created in `installations`
- Repeat with different `account_login` → row updated
- `installation_repositories` event → repos list updated
- Verify `updated_at` changes on upsert
