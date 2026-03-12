# GitHub App Installations

## Overview

Track which orgs/users have installed the GitHub App and which repos are accessible. Multi-tenant — the server handles installations across multiple GitHub accounts. Installation records are persisted to Postgres and updated reactively via webhook events.

## How Installations Are Tracked

The server does not poll GitHub for installation state. Instead, it reacts to two webhook event types:

| Event | Action | Effect |
|-------|--------|--------|
| `installation` | `created` | Upsert installation record |
| `installation` | `deleted` | Delete installation row |
| `installation` | `suspend` / `unsuspend` | Upsert (future: track suspension state) |
| `installation_repositories` | `added` | Merge new repos into existing set (dedup) |
| `installation_repositories` | `removed` | Remove repos from existing set |

These events are received by the webhook handler (see [webhook spec](1-webhook-receiver.md)) and processed inside the same transaction as event persistence. Routing lives in `webhook-handler.ts`; storage in `WebhookStore`.

## Database Schema

```sql
CREATE TABLE installations (
  installation_id BIGINT PRIMARY KEY,     -- GitHub's installation ID
  account_type    TEXT NOT NULL,           -- 'Organization' or 'User'
  account_login   TEXT NOT NULL,           -- org or user login name
  repos           JSONB,                   -- array of {full_name} objects, null = all repos
  created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
  updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
```

Created in the same migration as `webhook_events` (`0002_webhook_tables`).

### `repos` Column

- `null` — App installed on all repos in the account
- `[{"full_name": "org/repo-a"}, {"full_name": "org/repo-b"}]` — App installed on specific repos

## WebhookStore Methods

| Method | Purpose |
|--------|---------|
| `upsertInstallation` | Insert or update an installation record |
| `getInstallationRepos` | Read current repos for an installation (returns null if not found or all-repos) |
| `deleteInstallation` | Hard-delete an installation row |

### Upsert Logic

```
upsertInstallation({ installationId, accountType, accountLogin, repos }):
  INSERT INTO installations (installation_id, account_type, account_login, repos)
  VALUES ($1, $2, $3, $4)
  ON CONFLICT (installation_id) DO UPDATE SET
    account_type = EXCLUDED.account_type,
    account_login = EXCLUDED.account_login,
    repos = EXCLUDED.repos,
    updated_at = now()
```

When `repos` is null, SQL null is stored (not the string `"null"`).

## Event Handling

### `installation` events

- **`deleted` action** → `deleteInstallation(installationId)` — hard-deletes the row.
- **All other actions** (`created`, `suspend`, `unsuspend`, etc.) → `upsertInstallation` with `payload.repositories ?? null`. When the app is installed on all repos, GitHub omits `repositories` (or sends null), so repos is stored as `null`.

### `installation_repositories` events

Uses incremental merge rather than wholesale replacement, because GitHub's `installation_repositories` payload provides `repositories_added` / `repositories_removed` arrays:

1. Read current repos via `getInstallationRepos(installationId)`
2. If current repos is `null` (all-repos installation), keep `null` — no merge needed
3. If `action === "added"`: merge `repositories_added` into current set (dedup by `full_name`)
4. If `action === "removed"`: filter out `repositories_removed` from current set
5. Upsert with the resulting repos

### Payload Extraction

From `installation` events:
```
installationId  = payload.installation.id
accountType     = payload.installation.account.type      -- "Organization" | "User"
accountLogin    = payload.installation.account.login
repos           = payload.repositories                    -- null if "all repos" selection
```

From `installation_repositories` events:
```
installationId  = payload.installation.id
accountType     = payload.installation.account.type
accountLogin    = payload.installation.account.login
repos           = (incrementally merged from repositories_added / repositories_removed)
```

## Querying Installations

The CLI (or future server features) can query:

```sql
-- All installations for an org
SELECT * FROM installations WHERE account_login = 'my-org';

-- Check if a repo is covered
SELECT * FROM installations
WHERE repos IS NULL  -- all repos
   OR repos @> '[{"full_name": "org/repo-name"}]';
```

## Testing

**Integration** (in `tests/integration/webhook.test.ts`):
- `installation` event with `action: "created"` → row created in `installations`
- `installation` event with all repos (no `repositories` field) → `repos` is null in DB
- Duplicate delivery ID → idempotent, installation upserted only once
- `installation_repositories` + `added` → new repos merged into existing set
- `installation_repositories` + `removed` → repos removed from existing set
- `installation` + `deleted` → installation row deleted
- Upsert failure → 500, event row rolled back (transactional atomicity)
