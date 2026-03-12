import { Config, ConfigError, Context, Effect, Layer, Redacted } from "effect"

export class JwtSecret extends Context.Tag("JwtSecret")<JwtSecret, Uint8Array>() {}

export const JwtSecretLive: Layer.Layer<JwtSecret, ConfigError.ConfigError> = Layer.effect(
  JwtSecret,
  Effect.gen(function* () {
    const secret = yield* Config.redacted("BRRR_JWT_SECRET")
    const secretBytes = new TextEncoder().encode(Redacted.value(secret))
    if (secretBytes.length < 32) {
      return yield* Effect.fail(
        ConfigError.InvalidData(
          ["BRRR_JWT_SECRET"],
          "BRRR_JWT_SECRET must be at least 32 bytes (256 bits) for HS256",
        ),
      )
    }
    return secretBytes
  }),
)

export const ServerPort = Config.integer("BRRR_PORT").pipe(Config.withDefault(4000))
