import { Config, ConfigError, Either, Redacted } from "effect"

const JWT_SECRET_MIN_LENGTH = 32
const JWT_SECRET_TOO_SHORT_MESSAGE = "BRRR_JWT_SECRET must be at least 32 bytes (256 bits) for HS256"

export const JwtSecret = Config.redacted("BRRR_JWT_SECRET").pipe(
  Config.map(Redacted.value),
  Config.mapOrFail((secret) => {
    const secretBytes = new TextEncoder().encode(secret)

    return secretBytes.length < JWT_SECRET_MIN_LENGTH
      ? Either.left(ConfigError.InvalidData(["BRRR_JWT_SECRET"], JWT_SECRET_TOO_SHORT_MESSAGE))
      : Either.right(secretBytes)
  }),
)
