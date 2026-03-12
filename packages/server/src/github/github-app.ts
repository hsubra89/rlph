import { App } from "@octokit/app"
import { Octokit as RestOctokit } from "@octokit/rest"
import { Config, Context, Data, Effect, Layer, Redacted } from "effect"

type InstallationOctokit = InstanceType<typeof RestOctokit>

const GitHubAppClientId = Config.string("BRRR_GITHUB_APP_CLIENT_ID")

const GitHubAppPrivateKey = Config.redacted("BRRR_GITHUB_APP_PRIVATE_KEY").pipe(Config.map(Redacted.value))

export interface GitHubAppShape {
  readonly getInstallationOctokit: (
    installationId: number,
  ) => Effect.Effect<InstallationOctokit, GitHubApiError>
}

export class GitHubApp extends Context.Tag("GitHubApp")<GitHubApp, GitHubAppShape>() {}

export class GitHubApiError extends Data.TaggedError("GitHubApiError")<{
  readonly cause: unknown
}> {}

export const GitHubAppLive = Layer.effect(
  GitHubApp,
  Effect.gen(function* () {
    const clientId = yield* GitHubAppClientId
    const privateKey = yield* GitHubAppPrivateKey
    const app = new App({ appId: clientId, privateKey, Octokit: RestOctokit })

    return {
      getInstallationOctokit: (installationId: number) =>
        Effect.tryPromise({
          try: () => app.getInstallationOctokit(installationId),
          catch: (cause) => new GitHubApiError({ cause }),
        }),
    }
  }),
)
