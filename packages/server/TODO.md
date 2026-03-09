# TODO

- [ ] Bind auth to server identity to prevent cross-server replay
  - **JWT claims**: Add `iss`/`aud` to `SignJWT` in login handler, verify them in auth middleware. Without these, a JWT minted by server A is valid at server B if they share the same secret.
  - **Login signature payload**: Currently the client signs `username\nfingerprint\ntimestamp` — this isn't bound to any server. An attacker who intercepts a login request to server A can forward it to server B and get a valid JWT there. Fix: include the server's origin URL in the signed payload (e.g., `username\nfingerprint\ntimestamp\nhttps://brrr.example.com`) and verify it matches on the server side. This requires a coordinated change in the CLI and server.
- [ ] Add rate limiting on `/auth/login` to prevent DoS and GitHub API rate limit exhaustion
