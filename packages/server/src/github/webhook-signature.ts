import { Data, Effect } from "effect"
import * as crypto from "node:crypto"

export class WebhookSignatureError extends Data.TaggedError("WebhookSignatureError")<{
  readonly reason: string
}> {}

export const verifyWebhookSignature = (
  secret: string,
  signatureHeader: string,
  rawBody: string,
): Effect.Effect<void, WebhookSignatureError> =>
  Effect.gen(function* () {
    if (!signatureHeader.startsWith("sha256=")) {
      return yield* new WebhookSignatureError({ reason: "missing sha256= prefix" })
    }

    const expected = "sha256=" + crypto.createHmac("sha256", secret).update(rawBody).digest("hex")

    const sigBuf = Buffer.from(signatureHeader)
    const expectedBuf = Buffer.from(expected)

    if (sigBuf.length !== expectedBuf.length || !crypto.timingSafeEqual(sigBuf, expectedBuf)) {
      return yield* new WebhookSignatureError({ reason: "signature mismatch" })
    }
  })
