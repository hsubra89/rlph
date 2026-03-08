//! SSH challenge-response authentication against the brrr server.
//!
//! Flow: discover SSH key → get GitHub username → POST /auth/challenge → sign nonce → POST /auth/verify → store JWT.

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
struct ChallengeResponse {
    nonce: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct VerifyResponse {
    token: Option<String>,
    error: Option<String>,
}

/// Discovers the user's SSH public key, preferring ed25519.
fn discover_pubkey() -> Result<(PathBuf, String), Error> {
    let ssh_dir = dirs_path()?.join(".ssh");
    let candidates = ["id_ed25519.pub", "id_rsa.pub"];

    for name in &candidates {
        let path = ssh_dir.join(name);
        if path.exists() {
            let content = fs::read_to_string(&path)
                .map_err(|e| Error::Auth(format!("failed to read {}: {e}", path.display())))?;
            let pubkey = content.trim().to_string();
            if !pubkey.is_empty() {
                // Derive private key path (strip .pub)
                let private_key_path = path.with_extension("");
                debug!(key = %path.display(), "discovered SSH public key");
                return Ok((private_key_path, pubkey));
            }
        }
    }

    Err(Error::Auth(
        "no SSH public key found (~/.ssh/id_ed25519.pub or ~/.ssh/id_rsa.pub)".into(),
    ))
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

/// Runs the full challenge-response auth flow and stores the resulting JWT.
pub fn authenticate(server_url: &str) -> Result<String, Error> {
    let (private_key_path, pubkey) = discover_pubkey()?;
    let username = github_username()?;
    info!(username = %username, "authenticating with server");

    // Step 1: POST /auth/challenge
    let challenge_url = format!("{server_url}/auth/challenge");
    let challenge_body = serde_json::json!({
        "pubkey": pubkey,
        "username": username,
    });

    let challenge_resp: ChallengeResponse = ureq::post(&challenge_url)
        .send_json(&challenge_body)
        .map_err(|e| Error::Auth(format!("challenge request failed: {e}")))?
        .body_mut()
        .read_json()
        .map_err(|e| Error::Auth(format!("failed to parse challenge response: {e}")))?;

    if let Some(err) = challenge_resp.error {
        return Err(Error::Auth(format!("challenge rejected: {err}")));
    }

    let nonce = challenge_resp
        .nonce
        .ok_or_else(|| Error::Auth("challenge response missing nonce".into()))?;

    debug!("received nonce, signing with SSH key");

    // Step 2: Sign the nonce
    let signature = ssh_sign(&private_key_path, &nonce)?;

    // Step 3: POST /auth/verify
    let verify_url = format!("{server_url}/auth/verify");
    let verify_body = serde_json::json!({
        "pubkey": pubkey,
        "signature": signature,
    });

    let verify_resp: VerifyResponse = ureq::post(&verify_url)
        .send_json(&verify_body)
        .map_err(|e| Error::Auth(format!("verify request failed: {e}")))?
        .body_mut()
        .read_json()
        .map_err(|e| Error::Auth(format!("failed to parse verify response: {e}")))?;

    if let Some(err) = verify_resp.error {
        return Err(Error::Auth(format!("verification failed: {err}")));
    }

    let token = verify_resp
        .token
        .ok_or_else(|| Error::Auth("verify response missing token".into()))?;

    // Step 4: Store the token
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

    let session = Session {
        token: token.clone(),
        expires_at,
    };
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
    // Add padding
    let padded = match input.len() % 4 {
        2 => format!("{input}=="),
        3 => format!("{input}="),
        _ => input.to_string(),
    };
    // Replace URL-safe chars
    let standard = padded.replace('-', "+").replace('_', "/");
    // Use a simple base64 decoder
    let mut buf = Vec::new();
    base64_decode_impl(standard.as_bytes(), &mut buf)?;
    Some(buf)
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
}
