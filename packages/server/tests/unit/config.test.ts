import { describe, expect, it } from "@effect/vitest"
import { ConfigError, ConfigProvider, Effect } from "effect"
import { JwtSecret, JwtSecretLive, ServerPort } from "../../src/config.js"

const JWT_SECRET = "test-secret-that-is-at-least-32-bytes-long"

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

  it.effect("accepts secrets >= 32 bytes and encodes to Uint8Array", () =>
    Effect.gen(function* () {
      const secret = yield* JwtSecret
      expect(secret).toBeInstanceOf(Uint8Array)
      expect(secret.length).toBeGreaterThanOrEqual(32)
    }).pipe(
      Effect.provide(JwtSecretLive),
      Effect.withConfigProvider(ConfigProvider.fromMap(new Map([["BRRR_JWT_SECRET", JWT_SECRET]]))),
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

describe("ServerPort", () => {
  it.effect("defaults to 3000", () =>
    Effect.gen(function* () {
      const port = yield* ServerPort
      expect(port).toBe(3000)
    }).pipe(Effect.withConfigProvider(ConfigProvider.fromMap(new Map()))),
  )

  it.effect("reads BRRR_PORT", () =>
    Effect.gen(function* () {
      const port = yield* ServerPort
      expect(port).toBe(4000)
    }).pipe(Effect.withConfigProvider(ConfigProvider.fromMap(new Map([["BRRR_PORT", "4000"]])))),
  )
})
