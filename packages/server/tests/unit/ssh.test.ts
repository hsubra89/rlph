import { NodeCommandExecutor, NodeFileSystem } from "@effect/platform-node"
import { afterAll, beforeAll, describe, expect, it } from "@effect/vitest"
import { Effect, Either, Layer } from "effect"
import { execSync } from "node:child_process"
import * as fs from "node:fs"
import * as os from "node:os"
import * as path from "node:path"
import { FingerprintError, SshVerifyError, sshFingerprint, verifySshSignature } from "../../src/auth/ssh.js"

const TEST_PUBKEY =
  "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl test@example"

describe("sshFingerprint", () => {
  it("returns Right with SHA256 fingerprint for valid pubkey", () => {
    const result = sshFingerprint(TEST_PUBKEY)
    expect(Either.isRight(result)).toBe(true)
    if (Either.isRight(result)) {
      expect(result.right).toMatch(/^SHA256:/)
    }
  })

  it("returns Left for empty string", () => {
    const result = sshFingerprint("")
    expect(Either.isLeft(result)).toBe(true)
    if (Either.isLeft(result)) {
      expect(result.left).toBeInstanceOf(FingerprintError)
    }
  })

  it("returns Left for key type only (no key data)", () => {
    const result = sshFingerprint("ssh-ed25519")
    expect(Either.isLeft(result)).toBe(true)
  })

  it("trims surrounding whitespace", () => {
    const result = sshFingerprint(`  ${TEST_PUBKEY}  `)
    expect(Either.isRight(result)).toBe(true)
  })

  it("is deterministic", () => {
    expect(sshFingerprint(TEST_PUBKEY)).toEqual(sshFingerprint(TEST_PUBKEY))
  })
})

const PlatformLive = NodeCommandExecutor.layer.pipe(Layer.provideMerge(NodeFileSystem.layer))

describe("verifySshSignature", () => {
  let tmpDir: string
  let pubkey: string
  let signedData: string
  let validSignature: string

  beforeAll(() => {
    tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "brrr-ssh-test-"))
    const keyFile = path.join(tmpDir, "test_key")

    execSync(`ssh-keygen -t ed25519 -f ${keyFile} -N "" -q`)
    pubkey = fs.readFileSync(`${keyFile}.pub`, "utf-8").trim()

    signedData = "testuser\nSHA256:abc123\n1234567890"
    const dataFile = path.join(tmpDir, "data")
    fs.writeFileSync(dataFile, signedData)

    execSync(`ssh-keygen -Y sign -f ${keyFile} -n brrr ${dataFile}`)
    validSignature = fs.readFileSync(`${dataFile}.sig`, "utf-8")
  })

  afterAll(() => {
    if (tmpDir) fs.rmSync(tmpDir, { recursive: true, force: true })
  })

  it.live("succeeds with valid signature", () =>
    verifySshSignature(pubkey, validSignature, signedData).pipe(Effect.provide(PlatformLive)),
  )

  it.live("fails with signature_invalid for wrong data", () =>
    verifySshSignature(pubkey, validSignature, "wrong data").pipe(
      Effect.flip,
      Effect.tap((e) => {
        expect(e).toBeInstanceOf(SshVerifyError)
        expect(e.reason).toBe("signature_invalid")
      }),
      Effect.provide(PlatformLive),
    ),
  )

  it.live("fails with signature_invalid for tampered signature", () =>
    verifySshSignature(pubkey, "not a real signature", signedData).pipe(
      Effect.flip,
      Effect.tap((e) => {
        expect(e).toBeInstanceOf(SshVerifyError)
        expect(e.reason).toBe("signature_invalid")
      }),
      Effect.provide(PlatformLive),
    ),
  )
})
