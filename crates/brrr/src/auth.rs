//! Single-round-trip SSH authentication against the brrr server.
//!
//! Flow: discover SSH key → compute fingerprint → get GitHub username → sign `{username}\n{fingerprint}\n{timestamp}` → POST /auth/login → store JWT.

use std::fs::{self, DirBuilder};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use base64::prelude::*;
use sha2::{Digest, Sha256};

use crate::error::Error;

pub const DEFAULT_SERVER_URL: &str = "http://localhost:3000";

// ---------------------------------------------------------------------------
// AuthClient: authenticated requests with automatic 401 retry
// ---------------------------------------------------------------------------

/// Injectable authentication strategy.
pub trait Authenticator {
    fn authenticate(&self, server_url: &str) -> Result<String, Error>;
}

/// Production authenticator — runs the SSH-signed login flow.
pub struct SshAuthenticator;

impl Authenticator for SshAuthenticator {
    fn authenticate(&self, server_url: &str) -> Result<String, Error> {
        let (_username, token) = authenticate(server_url)?;
        Ok(token)
    }
}

/// Signals from the request closure back to [`AuthClient`].
pub enum RequestError {
    /// Server returned 401 — `AuthClient` will re-authenticate and retry once.
    Unauthorized,
    /// Any non-auth failure.
    Other(Error),
}

impl From<Error> for RequestError {
    fn from(e: Error) -> Self {
        RequestError::Other(e)
    }
}

/// Authenticated HTTP client with transparent 401 retry.
///
/// Manages a cached token and re-authenticates at most once per `request` call
/// when the server responds with 401 Unauthorized.
pub struct AuthClient<A> {
    server_url: String,
    token: std::cell::RefCell<Option<String>>,
    auth: A,
}

impl<A: Authenticator> AuthClient<A> {
    /// Creates a new client. Call [`AuthClient::with_cached_token`] to preload
    /// a token from disk.
    pub fn new(server_url: String, auth: A) -> Self {
        Self {
            server_url,
            token: std::cell::RefCell::new(None),
            auth,
        }
    }

    /// Creates a client with a pre-loaded token from disk (if available).
    pub fn with_cached_token(server_url: String, auth: A) -> Result<Self, Error> {
        let token = load_token()?;
        Ok(Self {
            server_url,
            token: std::cell::RefCell::new(token),
            auth,
        })
    }

    /// Execute `f` with a valid auth token. On [`RequestError::Unauthorized`],
    /// re-authenticates and retries `f` exactly once.
    pub fn request<F, T>(&self, f: F) -> Result<T, Error>
    where
        F: Fn(&str) -> Result<T, RequestError>,
    {
        let token = self.ensure_token()?;
        match f(&token) {
            Ok(val) => Ok(val),
            Err(RequestError::Unauthorized) => {
                debug!("received 401, re-authenticating");
                let token = self.refresh_token()?;
                match f(&token) {
                    Ok(val) => Ok(val),
                    Err(RequestError::Unauthorized) => {
                        Err(Error::Auth("unauthorized after re-authentication".into()))
                    }
                    Err(RequestError::Other(e)) => Err(e),
                }
            }
            Err(RequestError::Other(e)) => Err(e),
        }
    }

    fn ensure_token(&self) -> Result<String, Error> {
        if let Some(ref token) = *self.token.borrow() {
            return Ok(token.clone());
        }
        self.refresh_token()
    }

    fn refresh_token(&self) -> Result<String, Error> {
        let token = self.auth.authenticate(&self.server_url)?;
        *self.token.borrow_mut() = Some(token.clone());
        Ok(token)
    }
}
const JWT_FALLBACK_LIFETIME_SECS: u64 = 3600;

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

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
    let ssh_dir = home_path()?.join(".ssh");
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
    let home = home_path()?;
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

    let username = String::from_utf8(output.stdout)
        .map_err(|e| Error::Auth(format!("gh returned non-UTF-8 username: {e}")))?
        .trim()
        .to_string();
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

    if let Some(mut stdin) = child.stdin.take() {
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

    String::from_utf8(output.stdout)
        .map_err(|e| Error::Auth(format!("ssh-keygen returned non-UTF-8 output: {e}")))
}

/// Computes the SHA256 fingerprint of a public key string.
/// Mirrors the server-side `sshFingerprint()` in `ssh.ts`:
/// base64-decode the key data field, SHA-256 hash it, base64-encode (no padding).
fn ssh_fingerprint(pubkey: &str) -> Result<String, Error> {
    let key_data = pubkey
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| Error::Auth("public key has no key-data field".into()))?;

    let raw = BASE64_STANDARD
        .decode(key_data)
        .map_err(|_| Error::Auth("public key base64 decode failed".into()))?;

    let hash = Sha256::digest(&raw);
    let b64 = BASE64_STANDARD.encode(hash);
    let trimmed = b64.trim_end_matches('=');
    Ok(format!("SHA256:{trimmed}"))
}

fn session_path() -> Result<PathBuf, Error> {
    let config_dir = home_path()?.join(".config").join("brrr");
    Ok(config_dir.join("session.json"))
}

fn home_path() -> Result<PathBuf, Error> {
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

    let now = unix_now_secs();

    if session.expires_at <= now {
        debug!("stored token expired");
        return Ok(None);
    }

    Ok(Some(session.token))
}

/// Runs the single-round-trip auth flow and stores the resulting JWT.
/// Returns `(username, token)`.
pub fn authenticate(server_url: &str) -> Result<(String, String), Error> {
    let (private_key_path, pubkey) = discover_pubkey()?;
    let username = github_username()?;
    let fingerprint = ssh_fingerprint(&pubkey)?;
    info!(username = %username, "authenticating with server");

    let timestamp = unix_now_secs();

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
        let mut builder = DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder
            .create(parent)
            .map_err(|e| Error::Auth(format!("failed to create config dir: {e}")))?;
    }

    // Decode JWT to get expiry (simple base64 decode of payload)
    let expires_at = jwt_expiry(&token).unwrap_or_else(|| {
        // Fallback: 1 hour from now
        unix_now_secs() + JWT_FALLBACK_LIFETIME_SECS
    });

    let session = Session { token, expires_at };
    let json = serde_json::to_string_pretty(&session)
        .map_err(|e| Error::Auth(format!("failed to serialize session: {e}")))?;
    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut file = opts
            .open(&session_file)
            .map_err(|e| Error::Auth(format!("failed to open session file: {e}")))?;
        file.write_all(json.as_bytes())
            .map_err(|e| Error::Auth(format!("failed to write session file: {e}")))?;
    }

    info!(path = %session_file.display(), "session saved");
    let token = session.token;
    Ok((username, token))
}

/// Loads existing token or re-authenticates.
pub fn ensure_authenticated(server_url: &str) -> Result<String, Error> {
    if let Some(token) = load_token()? {
        debug!("using cached token");
        return Ok(token);
    }
    let (_username, token) = authenticate(server_url)?;
    Ok(token)
}

/// Extracts `exp` from a JWT payload (no signature verification, just parsing).
fn jwt_expiry(token: &str) -> Option<u64> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    // Base64url decode the payload
    let payload = BASE64_URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    value.get("exp")?.as_u64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Fake authenticator that returns predictable tokens.
    struct FakeAuth {
        call_count: AtomicU32,
    }

    impl FakeAuth {
        fn new() -> Self {
            Self {
                call_count: AtomicU32::new(0),
            }
        }
    }

    impl Authenticator for FakeAuth {
        fn authenticate(&self, _server_url: &str) -> Result<String, Error> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(format!("token-{n}"))
        }
    }

    #[test]
    fn auth_client_successful_request_passes_through() {
        let client = AuthClient::new("http://test".into(), FakeAuth::new());

        let result = client.request(|token| Ok(format!("got-{token}")));

        assert_eq!(result.unwrap(), "got-token-0");
    }

    #[test]
    fn auth_client_retries_on_401_with_fresh_token() {
        let client = AuthClient::new("http://test".into(), FakeAuth::new());
        let call_count = AtomicU32::new(0);

        let result = client.request(|token| {
            let n = call_count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // First call: simulate 401
                Err(RequestError::Unauthorized)
            } else {
                // Retry: should have a fresh token
                Ok(format!("got-{token}"))
            }
        });

        assert_eq!(result.unwrap(), "got-token-1");
        assert_eq!(call_count.load(Ordering::SeqCst), 2); // closure called twice
    }

    #[test]
    fn auth_client_uses_cached_token_without_authenticating() {
        let auth = FakeAuth::new();
        let client = AuthClient {
            server_url: "http://test".into(),
            token: std::cell::RefCell::new(Some("cached-token".into())),
            auth,
        };

        let result = client.request(|token| Ok(token.to_string()));

        assert_eq!(result.unwrap(), "cached-token");
        assert_eq!(client.auth.call_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn auth_client_returns_error_on_double_401() {
        let client = AuthClient::new("http://test".into(), FakeAuth::new());

        let result: Result<String, Error> =
            client.request(|_token| Err(RequestError::Unauthorized));

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unauthorized after re-authentication"),
            "{err}"
        );
    }

    #[test]
    fn auth_client_non_401_error_passes_through() {
        let client = AuthClient::new("http://test".into(), FakeAuth::new());

        let result: Result<String, Error> = client.request(|_token| {
            Err(RequestError::Other(Error::Auth(
                "connection refused".into(),
            )))
        });

        let err = result.unwrap_err().to_string();
        assert!(err.contains("connection refused"), "{err}");
    }

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
        let decoded = BASE64_URL_SAFE_NO_PAD.decode("SGVsbG8").unwrap();
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
        let expected_b64 = BASE64_STANDARD.encode(expected_hash);
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
