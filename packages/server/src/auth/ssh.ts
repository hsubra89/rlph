import * as crypto from "node:crypto"
import * as fs from "node:fs"
import * as os from "node:os"
import * as path from "node:path"
import { spawn } from "node:child_process"

export function sshFingerprint(pubkey: string): string {
  const parts = pubkey.trim().split(/\s+/)
  const keyData = parts[1]
  if (!keyData) return "unknown"
  const hash = crypto.createHash("sha256").update(Buffer.from(keyData, "base64")).digest("base64")
  return `SHA256:${hash.replace(/=+$/, "")}`
}

export function verifySshSignature(
  pubkey: string,
  signature: string,
  data: string,
): Promise<boolean> {
  return new Promise((resolve) => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "brrr-verify-"))
    const cleanup = () => fs.rmSync(tmpDir, { recursive: true, force: true })
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
        cleanup()
        resolve(code === 0)
      })

      proc.on("error", () => {
        cleanup()
        resolve(false)
      })
    } catch {
      cleanup()
      resolve(false)
    }
  })
}
