import { describe, expect, it } from "@effect/vitest"
import { ConfigError, ConfigProvider, Effect } from "effect"
import { JwtSecret } from "../../src/auth/jwt-secret.js"

describe("JwtSecret", () => {
  it.effect("rejects secrets shorter than 32 bytes", () =>
    Effect.gen(function* () {
      return yield* JwtSecret
    }).pipe(
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
      Effect.withConfigProvider(ConfigProvider.fromMap(new Map())),
      Effect.flip,
      Effect.tap((error) => {
        expect(ConfigError.isMissingData(error)).toBe(true)
      }),
    ),
  )
})
