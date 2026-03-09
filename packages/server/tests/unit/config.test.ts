import { describe, expect, it } from "@effect/vitest"
import { ConfigError, ConfigProvider, Effect, Redacted } from "effect"
import { AppConfigTag, AppConfigLiveLayer } from "../../src/config.js"

const JWT_SECRET = "test-secret-that-is-at-least-32-bytes-long"
const POSTGRES_URL = "postgres://postgres:postgres@127.0.0.1:5432/brrr"

const loadConfig = Effect.gen(function* () {
  return yield* AppConfigTag
}).pipe(Effect.provide(AppConfigLiveLayer))

describe("AppConfigLiveLayer", () => {
  it.effect("requires BRRR_POSTGRES_URL", () =>
    loadConfig.pipe(
      Effect.withConfigProvider(
        ConfigProvider.fromMap(
          new Map([
            ["BRRR_PORT", "4000"],
            ["BRRR_JWT_SECRET", JWT_SECRET],
          ]),
        ),
      ),
      Effect.flip,
      Effect.tap((error) => {
        expect(ConfigError.isMissingData(error)).toBe(true)
        expect(error).toEqual(
          ConfigError.MissingData(
            ["BRRR_POSTGRES_URL"],
            "Expected BRRR_POSTGRES_URL to exist in the provided map",
          ),
        )
      }),
    ),
  )

  it.effect("loads BRRR_POSTGRES_URL when provided", () =>
    loadConfig.pipe(
      Effect.withConfigProvider(
        ConfigProvider.fromMap(
          new Map([
            ["BRRR_PORT", "4000"],
            ["BRRR_JWT_SECRET", JWT_SECRET],
            ["BRRR_POSTGRES_URL", POSTGRES_URL],
          ]),
        ),
      ),
      Effect.tap((config) => {
        expect(config.port).toBe(4000)
        expect(Redacted.value(config.postgresUrl)).toBe(POSTGRES_URL)
      }),
    ),
  )
})
