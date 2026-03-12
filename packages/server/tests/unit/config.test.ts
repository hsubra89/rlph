import { describe, expect, it } from "@effect/vitest"
import { ConfigError, ConfigProvider, Effect } from "effect"
import { JwtSecret, JwtSecretLive } from "../../src/config.js"

describe("JwtSecretLive", () => {
  it.effect("rejects secrets shorter than 32 bytes", () =>
    Effect.gen(function* () {
      return yield* JwtSecret
    }).pipe(
      Effect.provide(JwtSecretLive),
      Effect.withConfigProvider(ConfigProvider.fromMap(new Map([["BRRR_JWT_SECRET", "too-short"]]))),
      Effect.flip,
      Effect.tap((error) => {
        expect(ConfigError.isInvalidData(error)).toBe(true)
      }),
    ),
  )

  it.effect("requires BRRR_JWT_SECRET", () =>
    Effect.gen(function* () {
      return yield* JwtSecret
    }).pipe(
      Effect.provide(JwtSecretLive),
      Effect.withConfigProvider(ConfigProvider.fromMap(new Map())),
      Effect.flip,
      Effect.tap((error) => {
        expect(ConfigError.isMissingData(error)).toBe(true)
      }),
    ),
  )
})
