import { HttpServerResponse } from "@effect/platform";
import { Effect } from "effect";
import { AuthClaims } from "./middleware.js";

export const handleWhoami = Effect.gen(function* () {
  const claims = yield* AuthClaims;
  return yield* HttpServerResponse.json({ ghuser: claims.ghuser, sub: claims.sub });
});
