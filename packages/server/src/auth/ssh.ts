import { Command, CommandExecutor, FileSystem } from "@effect/platform";
import { Data, Effect, Either } from "effect";
import * as crypto from "node:crypto";

export class FingerprintError extends Data.TaggedError("FingerprintError")<{}> {}

export class SshVerifyError extends Data.TaggedError("SshVerifyError")<{
  readonly reason: "spawn_failed" | "signature_invalid" | "setup_failed";
  readonly cause?: unknown;
}> {}

// eslint-disable-next-line no-control-regex
const controlCharRe = /[\x00-\x1f\x7f]/;

/** Parse and validate an SSH public key string. Returns [keyType, keyData] or null if invalid. */
function parseSshKey(pubkey: string): [string, string] | null {
  const parts = pubkey.trim().split(/\s+/);
  const [keyType, keyData] = parts;
  if (!keyType || !keyData) return null;
  if (controlCharRe.test(keyType) || controlCharRe.test(keyData)) return null;
  return [keyType, keyData];
}

export function sshFingerprint(pubkey: string): Either.Either<string, FingerprintError> {
  const parsed = parseSshKey(pubkey);
  if (!parsed) return Either.left(new FingerprintError());
  const hash = crypto.createHash("sha256").update(Buffer.from(parsed[1], "base64")).digest("base64");
  return Either.right(`SHA256:${hash.replace(/=+$/, "")}`);
}

export function verifySshSignature(
  pubkey: string,
  signature: string,
  data: string,
): Effect.Effect<void, SshVerifyError, FileSystem.FileSystem | CommandExecutor.CommandExecutor> {
  return Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem;

    const tmpDir = yield* fs
      .makeTempDirectoryScoped({ prefix: "brrr-verify-" })
      .pipe(Effect.mapError((e) => new SshVerifyError({ reason: "setup_failed", cause: e })));

    const sigFile = `${tmpDir}/sig`;
    const allowedSignersFile = `${tmpDir}/allowed_signers`;

    // Reject control characters (newlines, tabs, etc.) in key parts to prevent
    // injection of additional entries into the allowed_signers file.
    const keyParts = parseSshKey(pubkey);
    if (!keyParts) {
      return yield* Effect.fail(new SshVerifyError({ reason: "setup_failed" }));
    }

    const allowedSignerLine = `verify@brrr ${keyParts[0]} ${keyParts[1]}`;

    yield* fs
      .writeFileString(sigFile, signature)
      .pipe(Effect.mapError((e) => new SshVerifyError({ reason: "setup_failed", cause: e })));
    yield* fs
      .writeFileString(allowedSignersFile, allowedSignerLine + "\n")
      .pipe(Effect.mapError((e) => new SshVerifyError({ reason: "setup_failed", cause: e })));

    const cmd = Command.make(
      "ssh-keygen",
      "-Y",
      "verify",
      "-f",
      allowedSignersFile,
      "-I",
      "verify@brrr",
      "-n",
      "brrr",
      "-s",
      sigFile,
    ).pipe(Command.feed(data));

    const code = yield* Command.exitCode(cmd).pipe(
      Effect.mapError((e) => new SshVerifyError({ reason: "spawn_failed", cause: e })),
    );

    if (code !== 0) {
      return yield* Effect.fail(new SshVerifyError({ reason: "signature_invalid" }));
    }
  }).pipe(Effect.scoped);
}
