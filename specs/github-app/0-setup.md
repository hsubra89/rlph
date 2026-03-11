# GitHub App Setup

## Overview

Manual steps to create and configure the GitHub App before the server can receive webhooks or make API calls. This is a prerequisite for all other GitHub integration specs.

## 1. Create the GitHub App

Go to **GitHub Settings → Developer settings → GitHub Apps → New GitHub App**.

| Field | Value |
|-------|-------|
| App name | `brrr` (or `brrr-dev` for local) |
| Homepage URL | Repo URL or placeholder |
| Webhook URL | See [Callback URL](#2-callback-url) below |
| Webhook secret | Generate with `openssl rand -hex 32` → save as `BRRR_GITHUB_WEBHOOK_SECRET` |

## 2. Callback URL

### Local development

Use [smee.io](https://smee.io) to proxy webhooks to localhost:

```bash
# 1. Visit https://smee.io/new to get a unique channel URL
# 2. Set that URL as the Webhook URL in the GitHub App settings
# 3. Run the smee client locally:
npx smee -u https://smee.io/<channel-id> -t http://localhost:3000/webhooks/github
```

Alternative: [ngrok](https://ngrok.com):

```bash
ngrok http 3000
# Use the https://*.ngrok.io URL as the Webhook URL
```

### Production

Set the Webhook URL to your deployed server's public URL:

```
https://<your-domain>/webhooks/github
```

## 3. Permissions

Configure these **Repository permissions**:

| Permission | Access | Why |
|------------|--------|-----|
| Pull requests | Read & write | Read PR data, post review comments |
| Issues | Read & write | Read issues, post comments |
| Contents | Read-only | Read push/commit data |
| Checks | Read-only | Read check run / suite status |
| Commit statuses | Read-only | Read status events |
| Metadata | Read-only | Required (always on) |

## 4. Subscribe to Events

Check these webhook events:

- [x] Pull requests
- [x] Pull request reviews
- [x] Pull request review comments
- [x] Issues
- [x] Issue comments
- [x] Push
- [x] Check runs
- [x] Check suites
- [x] Statuses

## 5. Post-Creation: Collect Credentials

After creating the app, collect:

| Value | Where to find it | Env var |
|-------|-------------------|---------|
| Client ID | App settings page → "Client ID" | `BRRR_GITHUB_APP_CLIENT_ID` |
| Private key | App settings → "Generate a private key" (downloads .pem) | `BRRR_GITHUB_APP_PRIVATE_KEY` |
| Webhook secret | You set this in step 1 | `BRRR_GITHUB_WEBHOOK_SECRET` |

For the private key env var, set the full PEM content:

```bash
export BRRR_GITHUB_APP_PRIVATE_KEY="$(cat ~/Downloads/brrr.2024-01-01.private-key.pem)"
```

## 6. Install the App

Go to **App settings → Install App** and install on the target org/user account. Choose "All repositories" or select specific repos.

This triggers an `installation` event → the server persists the installation record (see [installations spec](2-installations.md)).

## 7. Local Dev .env

```bash
# .env (not committed)
BRRR_GITHUB_APP_CLIENT_ID=Iv1.abc123def456
BRRR_GITHUB_APP_PRIVATE_KEY="-----BEGIN RSA PRIVATE KEY-----\n...\n-----END RSA PRIVATE KEY-----"
BRRR_GITHUB_WEBHOOK_SECRET=<hex string from step 1>
BRRR_POSTGRES_URL=postgres://localhost:5432/brrr
BRRR_JWT_SECRET=<at least 32 bytes>
BRRR_PORT=3000
```

## Implementation Order

0. **This setup** — create the GitHub App, collect credentials, configure smee for local dev
1. **[Webhook receiver](1-webhook-receiver.md)** — HMAC verification, raw event storage
2. **[Installations](2-installations.md)** — track installed orgs/repos via webhook events
3. **[API client](3-api-client.md)** — outbound GitHub API calls via installation tokens
