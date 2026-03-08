import { Config, ConfigError, Effect, Layer, Redacted } from "effect"
import * as Context from "effect/Context"

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
    return new AppConfig(port, secretBytes)
  }),
)
