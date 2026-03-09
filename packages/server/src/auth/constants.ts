import { Duration } from "effect"

const JWT_LIFETIME_HOURS = 1

/** JWT lifetime string for jose's setExpirationTime. */
export const JWT_EXPIRY = `${JWT_LIFETIME_HOURS}h`

/** JWT lifetime as an Effect Duration. Used for cache TTL in TokenDenylist. */
export const JWT_LIFETIME_DURATION = Duration.hours(JWT_LIFETIME_HOURS)

/** Freshness window in seconds. Replay guard TTL must cover this window. */
export const TIMESTAMP_FRESHNESS_SECS = 60

/** Current Unix time in seconds. */
export const unixNowSecs = (): number => Math.floor(Date.now() / 1000)
