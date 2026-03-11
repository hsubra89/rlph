# GitHub API Client

## Overview

An Effect service wrapping `@octokit/app` that provides authenticated GitHub API access using GitHub App installation tokens. Enables the server (and future features) to read repo data and post comments/statuses/PRs on behalf of the installed app.

## Why Octokit

- Official GitHub SDK, handles JWT signing for app authentication
- Automatic installation token caching and renewal (tokens expire after 1 hour)
- Built-in pagination, rate limit handling, and retry logic
- Type-safe API methods

## Service Definition

```ts
interface GitHubAppShape {
  readonly getInstallationOctokit: (
    installationId: number
  ) => Effect.Effect<Octokit, GitHubApiError>
}

class GitHubApp extends Context.Tag("GitHubApp")<GitHubApp, GitHubAppShape>() {}

class GitHubApiError extends Data.TaggedError("GitHubApiError")<{
  readonly cause: unknown
}> {}
```

## Layer Construction

```ts
GitHubAppLive = Layer.effect(GitHubApp, Effect.gen(function* () {
  const clientId = yield* GitHubAppClientId
  const privateKey = yield* GitHubAppPrivateKey
  const app = new App({ appId: clientId, privateKey })

  return {
    getInstallationOctokit: (installationId) =>
      Effect.tryPromise({
        try: () => app.getInstallationOctokit(installationId),
        catch: (cause) => new GitHubApiError({ cause }),
      }),
  }
}))
```

The `App` instance is created once at layer construction. `@octokit/app` internally:
- Signs a JWT with the app's private key for app-level auth
- Exchanges it for an installation token on first `getInstallationOctokit` call
- Caches the token and renews it before expiry

## Configuration

| Env var | Required | Description |
|---------|----------|-------------|
| `BRRR_GITHUB_APP_CLIENT_ID` | Yes | GitHub App Client ID (e.g. `Iv1.abc123def456`) |
| `BRRR_GITHUB_APP_PRIVATE_KEY` | Yes | PEM-encoded private key (full content, not path) |

Provided as Effect services:

```ts
class GitHubAppClientId extends Context.Tag("GitHubAppClientId")<GitHubAppClientId, string>() {}
GitHubAppClientIdLive = Layer.effect(GitHubAppClientId, Config.string("BRRR_GITHUB_APP_CLIENT_ID"))

class GitHubAppPrivateKey extends Context.Tag("GitHubAppPrivateKey")<GitHubAppPrivateKey, string>() {}
GitHubAppPrivateKeyLive = Layer.effect(GitHubAppPrivateKey,
  Effect.map(Config.redacted("BRRR_GITHUB_APP_PRIVATE_KEY"), Redacted.value))
```

These config tags live in `src/github/config.ts` alongside `GitHubWebhookSecret`.

## Dependencies

Add to `packages/server/package.json`:

```json
"@octokit/app": "^15.1.1",
"@octokit/rest": "^21.1.1"
```

`@octokit/rest` is a transitive dep but explicitly listed for direct `Octokit` type imports.

## New Files

| File | Purpose |
|------|---------|
| `src/github/github-app.ts` | `GitHubApp` service, `GitHubAppLive` layer, `GitHubApiError` |

## Modified Files

| File | Change |
|------|--------|
| `src/github/config.ts` | Add `GitHubAppClientId` + `GitHubAppPrivateKey` tags and live layers |
| `package.json` | Add `@octokit/app`, `@octokit/rest` deps |

## Wiring

The `GitHubApp` service is **not wired into the webhook handler** — webhooks are store-only. The service exists for future use:
- CLI polling endpoint that needs to fetch PR details
- Status check posting
- Comment posting

When needed, add `GitHubAppLive` (+ `GitHubAppClientIdLive`, `GitHubAppPrivateKeyLive`) to the `main.ts` layer chain.

## Usage Example

```ts
const app = yield* GitHubApp
const octokit = yield* app.getInstallationOctokit(installationId)
const { data: pr } = yield* Effect.tryPromise({
  try: () => octokit.pulls.get({ owner: "org", repo: "repo", pull_number: 42 }),
  catch: (cause) => new GitHubApiError({ cause }),
})
```

## Testing

Unit testing the service itself is low value (it's a thin wrapper). Test via integration when features that use it are built. For now, ensure:
- `GitHubAppLive` layer constructs without error given valid config
- `GitHubApiError` is properly tagged
