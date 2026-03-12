import { describe, expect, it } from "@effect/vitest"
import { ConfigError, ConfigProvider, Effect } from "effect"
import { generateKeyPairSync } from "node:crypto"
import { GitHubApiError, GitHubApp, GitHubAppLive } from "../../src/github/github-app.js"

const TEST_CLIENT_ID = "Iv1.test1234567890"
const TEST_PRIVATE_KEY = generateKeyPairSync("rsa", {
  modulusLength: 2048,
}).privateKey.export({ type: "pkcs1", format: "pem" })

describe("GitHub app config", () => {
  it.effect("requires BRRR_GITHUB_APP_CLIENT_ID", () =>
    Effect.gen(function* () {
      return yield* GitHubApp
    }).pipe(
      Effect.provide(GitHubAppLive),
      Effect.withConfigProvider(
        ConfigProvider.fromMap(new Map([["BRRR_GITHUB_APP_PRIVATE_KEY", TEST_PRIVATE_KEY]])),
      ),
      Effect.flip,
      Effect.tap((error) => {
        expect(ConfigError.isMissingData(error)).toBe(true)
      }),
    ),
  )

  it.effect("requires BRRR_GITHUB_APP_PRIVATE_KEY", () =>
    Effect.gen(function* () {
      return yield* GitHubApp
    }).pipe(
      Effect.provide(GitHubAppLive),
      Effect.withConfigProvider(ConfigProvider.fromMap(new Map())),
      Effect.flip,
      Effect.tap((error) => {
        expect(ConfigError.isMissingData(error)).toBe(true)
      }),
    ),
  )

  it.effect("constructs when both config values are present", () =>
    Effect.gen(function* () {
      const githubApp = yield* GitHubApp
      expect(typeof githubApp.getInstallationOctokit).toBe("function")
    }).pipe(
      Effect.provide(GitHubAppLive),
      Effect.withConfigProvider(
        ConfigProvider.fromMap(
          new Map([
            ["BRRR_GITHUB_APP_CLIENT_ID", TEST_CLIENT_ID],
            ["BRRR_GITHUB_APP_PRIVATE_KEY", TEST_PRIVATE_KEY],
          ]),
        ),
      ),
    ),
  )
})

describe("GitHubAppLive", () => {
  it("tags GitHubApiError", () => {
    const cause = new Error("boom")
    const error = new GitHubApiError({ cause })

    expect(error._tag).toBe("GitHubApiError")
    expect(error.cause).toBe(cause)
  })
})
