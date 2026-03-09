import { Config, ConfigError, Context, Effect, Layer, Redacted } from "effect"

export class AppConfig {
  constructor(
    readonly port: number,
    readonly jwtSecret: Uint8Array,
  ) {}
}

export class AppConfigTag extends Context.Tag("AppConfig")<AppConfigTag, AppConfig>() {}

export const AppConfigLiveLayer: Layer.Layer<AppConfigTag, ConfigError.ConfigError> = Layer.effect(
  AppConfigTag,
  Effect.gen(function* () {
    const port = yield* Config.integer("BRRR_PORT").pipe(Config.withDefault(3000))
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
    return new AppConfig(port, secretBytes)
  }),
)
