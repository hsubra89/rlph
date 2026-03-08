import { HttpServerRequest, HttpServerResponse } from "@effect/platform"
import { Effect } from "effect"
import * as crypto from "node:crypto"
import * as fs from "node:fs"
import * as os from "node:os"
import * as path from "node:path"
import { spawn } from "node:child_process"
import * as jose from "jose"
import { nonceStore } from "./nonce-store.js"

function sshFingerprint(pubkey: string): string {
  const parts = pubkey.trim().split(/\s+/)
  const keyData = parts[1]
  if (!keyData) return "unknown"
  const hash = crypto.createHash("sha256").update(Buffer.from(keyData, "base64")).digest("base64")
  return `SHA256:${hash.replace(/=+$/, "")}`
}

function verifySshSignature(
  pubkey: string,
  signature: string,
  data: string,
): Promise<boolean> {
  return new Promise((resolve) => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "brrr-verify-"))
    const sigFile = path.join(tmpDir, "sig")
    const allowedSignersFile = path.join(tmpDir, "allowed_signers")

    try {
      fs.writeFileSync(sigFile, signature)

      const keyParts = pubkey.trim().split(/\s+/)
      const allowedSignerLine = `verify@brrr ${keyParts[0]} ${keyParts[1]}`
      fs.writeFileSync(allowedSignersFile, allowedSignerLine + "\n")

      const proc = spawn("ssh-keygen", [
        "-Y", "verify",
        "-f", allowedSignersFile,
        "-I", "verify@brrr",
        "-n", "brrr",
        "-s", sigFile,
      ], { stdio: ["pipe", "pipe", "pipe"] })

      proc.stdin.write(data)
      proc.stdin.end()

      proc.on("close", (code) => {
        fs.rmSync(tmpDir, { recursive: true, force: true })
        resolve(code === 0)
      })

      proc.on("error", () => {
        fs.rmSync(tmpDir, { recursive: true, force: true })
        resolve(false)
      })
    } catch {
      fs.rmSync(tmpDir, { recursive: true, force: true })
      resolve(false)
    }
  })
}

export const makeHandleVerify = (jwtSecret: Uint8Array) =>
  Effect.gen(function* () {
    const request = yield* HttpServerRequest.HttpServerRequest
    const body = yield* request.json as Effect.Effect<
      { pubkey: string; signature: string },
      unknown
    >

    const { pubkey, signature } = body as { pubkey: string; signature: string }

    if (!pubkey || !signature) {
      return yield* HttpServerResponse.json(
        { error: "pubkey and signature required" },
        { status: 400 },
      )
    }

    // Look up nonce by pubkey
    const entry = nonceStore.findByPubkey(pubkey.trim())
    if (!entry) {
      return yield* HttpServerResponse.json(
        { error: "No pending challenge for this key" },
        { status: 401 },
      )
    }

    // Verify the SSH signature using ssh-keygen
    const verified = yield* Effect.tryPromise({
      try: () => verifySshSignature(entry.pubkey, signature, entry.nonce),
      catch: () => new Error("Signature verification failed"),
    })

    if (!verified) {
      return yield* HttpServerResponse.json(
        { error: "Signature verification failed" },
        { status: 401 },
      )
    }

    // Delete nonce (one-time use)
    nonceStore.delete(entry.nonce)

    // Issue JWT
    const fingerprint = sshFingerprint(entry.pubkey)
    const token = yield* Effect.tryPromise({
      try: () =>
        new jose.SignJWT({ ghuser: entry.username })
          .setProtectedHeader({ alg: "HS256" })
          .setSubject(fingerprint)
          .setIssuedAt()
          .setExpirationTime("1h")
          .sign(jwtSecret),
      catch: (e) => new Error(`JWT signing failed: ${e}`),
    })

    return yield* HttpServerResponse.json({ token })
  })
