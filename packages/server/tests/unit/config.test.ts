import { describe, expect, it } from "@effect/vitest"
import { ConfigError, ConfigProvider, Effect, Redacted } from "effect"
import { JwtSecret, JwtSecretLive, ServerPort } from "../../src/config.js"
import { PostgresMigrationsUrl } from "../../src/database.js"
import { TEST_JWT_SECRET_RAW } from "../helpers/constants.js"

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
      Effect.withConfigProvider(ConfigProvider.fromMap(new Map([["BRRR_JWT_SECRET", TEST_JWT_SECRET_RAW]]))),
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

describe("PostgresMigrationsUrl", () => {
  it.effect("uses BRRR_POSTGRES_MIGRATIONS_URL when set", () =>
    Effect.gen(function* () {
      const url = yield* PostgresMigrationsUrl
      expect(Redacted.value(url)).toBe("postgres://migrations")
    }).pipe(
      Effect.withConfigProvider(
        ConfigProvider.fromMap(
          new Map([
            ["BRRR_POSTGRES_URL", "postgres://runtime"],
            ["BRRR_POSTGRES_MIGRATIONS_URL", "postgres://migrations"],
          ]),
        ),
      ),
    ),
  )

  it.effect("falls back to BRRR_POSTGRES_URL", () =>
    Effect.gen(function* () {
      const url = yield* PostgresMigrationsUrl
      expect(Redacted.value(url)).toBe("postgres://runtime")
    }).pipe(
      Effect.withConfigProvider(ConfigProvider.fromMap(new Map([["BRRR_POSTGRES_URL", "postgres://runtime"]]))),
    ),
  )
})
