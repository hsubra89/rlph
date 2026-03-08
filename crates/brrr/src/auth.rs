//! Single-round-trip SSH authentication against the brrr server.
//!
//! Flow: discover SSH key → compute fingerprint → get GitHub username → sign `{username}\n{fingerprint}\n{timestamp}` → POST /auth/login → store JWT.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use crate::error::Error;

#[derive(Debug, Serialize, Deserialize)]
struct Session {
    token: String,
    expires_at: u64,
}

#[derive(Debug, Deserialize)]
struct LoginResponse {
    token: Option<String>,
    error: Option<String>,
}

/// Discovers the SSH key used for `github.com` by querying the resolved SSH config
/// via `ssh -G github.com`. Falls back to `~/.ssh/id_ed25519` / `id_rsa` if that fails.
fn discover_pubkey() -> Result<(PathBuf, String), Error> {
    if let Some(result) = discover_pubkey_from_ssh_config()? {
        return Ok(result);
    }

    // Fallback: default key names
    let ssh_dir = dirs_path()?.join(".ssh");
    let candidates = ["id_ed25519.pub", "id_rsa.pub"];

    for name in &candidates {
        let path = ssh_dir.join(name);
        if let Some(result) = try_read_keypair(&path)? {
            return Ok(result);
        }
    }

    Err(Error::Auth(
        "no SSH key for github.com found (checked `ssh -G github.com` and ~/.ssh/id_{ed25519,rsa})"
            .into(),
    ))
}

/// Parses `ssh -G github.com` output for `identityfile` lines and returns the
/// first keypair where the `.pub` file exists on disk.
fn discover_pubkey_from_ssh_config() -> Result<Option<(PathBuf, String)>, Error> {
    let output = match Command::new("ssh").args(["-G", "github.com"]).output() {
        Ok(o) if o.status.success() => o,
        _ => {
            debug!("`ssh -G github.com` failed, falling back to default key paths");
            return Ok(None);
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let home = dirs_path()?;
    let candidates = parse_identity_files(&stdout, &home);

    for pub_path in candidates {
        if let Some(result) = try_read_keypair(&pub_path)? {
            return Ok(Some(result));
        }
    }

    Ok(None)
}

/// Extracts identity file `.pub` paths from `ssh -G` output, expanding `~/` against `home`.
/// Pure function — no I/O.
fn parse_identity_files(ssh_config_output: &str, home: &Path) -> Vec<PathBuf> {
    ssh_config_output
        .lines()
        .filter_map(|line| {
            let raw_path = line.trim().strip_prefix("identityfile ")?;
            let expanded = if let Some(rest) = raw_path.strip_prefix("~/") {
                home.join(rest)
            } else {
                PathBuf::from(raw_path)
            };
            // Skip paths that already have an extension (e.g. `.pem` cert files)
            if expanded.extension().is_some() {
                return None;
            }
            Some(expanded.with_extension("pub"))
        })
        .collect()
}

/// Reads a `.pub` file and returns `(private_key_path, pubkey_contents)` if the file
/// exists and is non-empty.
fn try_read_keypair(pub_path: &Path) -> Result<Option<(PathBuf, String)>, Error> {
    if !pub_path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(pub_path)
        .map_err(|e| Error::Auth(format!("failed to read {}: {e}", pub_path.display())))?;
    let pubkey = content.trim().to_string();
    if pubkey.is_empty() {
        return Ok(None);
    }
    let private_key_path = pub_path.with_extension("");
    debug!(key = %pub_path.display(), "discovered SSH public key");
    Ok(Some((private_key_path, pubkey)))
}

/// Gets the GitHub username via `gh api user`.
fn github_username() -> Result<String, Error> {
    let output = Command::new("gh")
        .args(["api", "user", "-q", ".login"])
        .output()
        .map_err(|e| Error::Auth(format!("failed to run `gh api user`: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Auth(format!(
            "`gh api user` failed: {stderr}. Is `gh` installed and authenticated?"
        )));
    }

    let username = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if username.is_empty() {
        return Err(Error::Auth("gh returned empty username".into()));
    }
    Ok(username)
}

/// Signs data using `ssh-keygen -Y sign`.
fn ssh_sign(private_key_path: &Path, data: &str) -> Result<String, Error> {
    let mut child = Command::new("ssh-keygen")
        .args([
            "-Y",
            "sign",
            "-f",
            &private_key_path.to_string_lossy(),
            "-n",
            "brrr",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::Auth(format!("failed to spawn ssh-keygen: {e}")))?;

    if let Some(ref mut stdin) = child.stdin {
        stdin
            .write_all(data.as_bytes())
            .map_err(|e| Error::Auth(format!("failed to write to ssh-keygen stdin: {e}")))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| Error::Auth(format!("ssh-keygen failed: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Auth(format!("ssh-keygen signing failed: {stderr}")));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Computes the SHA256 fingerprint of a public key string.
/// Mirrors the server-side `sshFingerprint()` in `ssh.ts`:
/// base64-decode the key data field, SHA-256 hash it, base64-encode (no padding).
fn ssh_fingerprint(pubkey: &str) -> Result<String, Error> {
    let key_data = pubkey
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| Error::Auth("public key has no key-data field".into()))?;

    let raw = base64url_decode_standard(key_data)
        .ok_or_else(|| Error::Auth("public key base64 decode failed".into()))?;

    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(&raw);
    let b64 = base64_encode_standard(&hash);
    let trimmed = b64.trim_end_matches('=');
    Ok(format!("SHA256:{trimmed}"))
}

/// Standard base64 encode (not URL-safe).
fn base64_encode_standard(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 0x3F) as usize] as char);
        out.push(TABLE[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Standard base64 decode (not URL-safe).
fn base64url_decode_standard(input: &str) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    base64_decode_impl(input.as_bytes(), &mut buf)?;
    Some(buf)
}

fn session_path() -> Result<PathBuf, Error> {
    let config_dir = dirs_path()?.join(".config").join("brrr");
    Ok(config_dir.join("session.json"))
}

fn dirs_path() -> Result<PathBuf, Error> {
    dirs::home_dir().ok_or_else(|| Error::Auth("cannot determine home directory".into()))
}

/// Loads a stored token if it exists and hasn't expired.
pub fn load_token() -> Result<Option<String>, Error> {
    let path = session_path()?;
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path)
        .map_err(|e| Error::Auth(format!("failed to read session file: {e}")))?;
    let session: Session = serde_json::from_str(&content)
        .map_err(|e| Error::Auth(format!("failed to parse session file: {e}")))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    if session.expires_at <= now {
        debug!("stored token expired");
        return Ok(None);
    }

    Ok(Some(session.token))
}

/// Runs the single-round-trip auth flow and stores the resulting JWT.
pub fn authenticate(server_url: &str) -> Result<String, Error> {
    let (private_key_path, pubkey) = discover_pubkey()?;
    let username = github_username()?;
    let fingerprint = ssh_fingerprint(&pubkey)?;
    info!(username = %username, "authenticating with server");

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let payload = format!("{username}\n{fingerprint}\n{timestamp}");
    let signature = ssh_sign(&private_key_path, &payload)?;

    debug!("signed payload, sending login request");

    let login_url = format!("{server_url}/auth/login");
    let login_body = serde_json::json!({
        "username": username,
        "fingerprint": fingerprint,
        "timestamp": timestamp,
        "signature": signature,
    });

    let login_resp: LoginResponse = ureq::post(&login_url)
        .send_json(&login_body)
        .map_err(|e| Error::Auth(format!("login request failed: {e}")))?
        .body_mut()
        .read_json()
        .map_err(|e| Error::Auth(format!("failed to parse login response: {e}")))?;

    if let Some(err) = login_resp.error {
        return Err(Error::Auth(format!("login failed: {err}")));
    }

    let token = login_resp
        .token
        .ok_or_else(|| Error::Auth("login response missing token".into()))?;

    // Persist session to disk
    let session_file = session_path()?;
    if let Some(parent) = session_file.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| Error::Auth(format!("failed to create config dir: {e}")))?;
    }

    // Decode JWT to get expiry (simple base64 decode of payload)
    let expires_at = jwt_expiry(&token).unwrap_or_else(|| {
        // Fallback: 1 hour from now
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600
    });

    let session = Session { token, expires_at };
    let json = serde_json::to_string_pretty(&session)
        .map_err(|e| Error::Auth(format!("failed to serialize session: {e}")))?;
    fs::write(&session_file, json)
        .map_err(|e| Error::Auth(format!("failed to write session file: {e}")))?;

    info!(path = %session_file.display(), "session saved");
    Ok(username)
}

/// Loads existing token or re-authenticates.
pub fn ensure_authenticated(server_url: &str) -> Result<String, Error> {
    if let Some(token) = load_token()? {
        debug!("using cached token");
        return Ok(token);
    }
    authenticate(server_url)?;
    load_token()?.ok_or_else(|| Error::Auth("authentication succeeded but token not found".into()))
}

/// Extracts `exp` from a JWT payload (no signature verification, just parsing).
fn jwt_expiry(token: &str) -> Option<u64> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    // Base64url decode the payload
    let payload = base64url_decode(parts[1])?;
    let value: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    value.get("exp")?.as_u64()
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    // URL-safe → standard: replace chars and add padding
    let standard = input.replace('-', "+").replace('_', "/");
    let padded = match standard.len() % 4 {
        2 => format!("{standard}=="),
        3 => format!("{standard}="),
        _ => standard,
    };
    base64url_decode_standard(&padded)
}

fn base64_decode_impl(input: &[u8], output: &mut Vec<u8>) -> Option<()> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (i, &c) in TABLE.iter().enumerate() {
        lookup[c as usize] = i as u8;
    }

    let mut buf = 0u32;
    let mut bits = 0;
    for &byte in input {
        if byte == b'=' {
            break;
        }
        let val = lookup[byte as usize];
        if val == 255 {
            return None;
        }
        buf = (buf << 6) | val as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jwt_expiry_extraction() {
        // A test JWT with exp=1709903600
        // Header: {"alg":"HS256","typ":"JWT"}
        // Payload: {"sub":"SHA256:abc","ghuser":"testuser","iat":1709900000,"exp":1709903600}
        let header = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";
        let payload = "eyJzdWIiOiJTSEEyNTY6YWJjIiwiZ2h1c2VyIjoidGVzdHVzZXIiLCJpYXQiOjE3MDk5MDAwMDAsImV4cCI6MTcwOTkwMzYwMH0";
        let sig = "dummysig";
        let token = format!("{header}.{payload}.{sig}");

        assert_eq!(jwt_expiry(&token), Some(1709903600));
    }

    #[test]
    fn test_jwt_expiry_invalid_token() {
        assert_eq!(jwt_expiry("not.a.valid.token"), None);
        assert_eq!(jwt_expiry(""), None);
    }

    #[test]
    fn test_base64url_decode() {
        let decoded = base64url_decode("SGVsbG8").unwrap();
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn test_session_path() {
        let path = session_path().unwrap();
        assert!(path.to_string_lossy().contains("session.json"));
    }

    #[test]
    fn test_ssh_fingerprint_from_pubkey_string() {
        // A known ed25519 public key (32 bytes of zeros, base64 = AAAA...AA==)
        // The key-data is base64-encoded; we SHA-256 that decoded blob, then base64 the hash.
        let pubkey =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeKeyDataForTestingPurposesOnly00 user@host";
        let fp = ssh_fingerprint(pubkey).unwrap();
        assert!(fp.starts_with("SHA256:"));
        assert!(!fp.ends_with('='));
    }

    #[test]
    fn test_ssh_fingerprint_no_key_data() {
        assert!(ssh_fingerprint("ssh-ed25519").is_err());
    }

    #[test]
    fn test_ssh_fingerprint_matches_typescript_logic() {
        // Verify our Rust implementation matches the TS sshFingerprint():
        // both do SHA256(base64_decode(key_data)) then base64-encode with no padding.
        // Use a trivial key_data "AAAA" which decodes to [0, 0, 0].
        let pubkey = "ssh-rsa AAAA comment";
        let fp = ssh_fingerprint(pubkey).unwrap();
        // SHA256 of [0,0,0] is a known hash
        use sha2::{Digest, Sha256};
        let expected_hash = Sha256::digest([0u8, 0, 0]);
        let expected_b64 = base64_encode_standard(&expected_hash);
        let expected = format!("SHA256:{}", expected_b64.trim_end_matches('='));
        assert_eq!(fp, expected);
    }

    #[test]
    fn test_parse_identity_files_typical_output() {
        let output = "\
user harish
hostname github.com
port 22
identityfile ~/.ssh/id_rsa
identityfile ~/.ssh/id_ecdsa
identityfile ~/.ssh/id_ed25519
";
        let home = PathBuf::from("/home/alice");
        let paths = parse_identity_files(output, &home);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("/home/alice/.ssh/id_rsa.pub"),
                PathBuf::from("/home/alice/.ssh/id_ecdsa.pub"),
                PathBuf::from("/home/alice/.ssh/id_ed25519.pub"),
            ]
        );
    }

    #[test]
    fn test_parse_identity_files_custom_key() {
        let output = "\
hostname github.com
identityfile ~/.ssh/github_deploy
";
        let home = PathBuf::from("/Users/bob");
        let paths = parse_identity_files(output, &home);
        assert_eq!(
            paths,
            vec![PathBuf::from("/Users/bob/.ssh/github_deploy.pub")]
        );
    }

    #[test]
    fn test_parse_identity_files_absolute_path() {
        let output = "identityfile /etc/ssh/custom_key\n";
        let home = PathBuf::from("/home/x");
        let paths = parse_identity_files(output, &home);
        assert_eq!(paths, vec![PathBuf::from("/etc/ssh/custom_key.pub")]);
    }

    #[test]
    fn test_parse_identity_files_skips_cert_files() {
        let output = "\
identityfile ~/.ssh/id_ed25519
identityfile ~/.ssh/id_ed25519-cert.pem
";
        let home = PathBuf::from("/home/x");
        let paths = parse_identity_files(output, &home);
        assert_eq!(paths, vec![PathBuf::from("/home/x/.ssh/id_ed25519.pub")]);
    }

    #[test]
    fn test_parse_identity_files_empty_output() {
        let paths = parse_identity_files("", &PathBuf::from("/home/x"));
        assert!(paths.is_empty());
    }

    #[test]
    fn test_parse_identity_files_no_identity_lines() {
        let output = "hostname github.com\nport 22\nuser git\n";
        let paths = parse_identity_files(output, &PathBuf::from("/home/x"));
        assert!(paths.is_empty());
    }
}
