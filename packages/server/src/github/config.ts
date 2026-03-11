import { Config, ConfigError, Context, Effect, Layer, Redacted } from "effect"

export class GitHubWebhookSecret extends Context.Tag("GitHubWebhookSecret")<GitHubWebhookSecret, string>() {}

export const GitHubWebhookSecretLive: Layer.Layer<GitHubWebhookSecret, ConfigError.ConfigError> =
  Layer.effect(
    GitHubWebhookSecret,
    Effect.gen(function* () {
      const secret = yield* Config.redacted("BRRR_GITHUB_WEBHOOK_SECRET")
      return Redacted.value(secret)
    }),
  )
