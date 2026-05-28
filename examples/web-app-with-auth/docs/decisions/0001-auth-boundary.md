---
status: active
authority: canonical
scope_paths:
  - src/server/auth/
---

# Decision: Auth Boundary

Password verification and session issuance stay on the server.

## Decision

The browser can submit credentials and display login state, but all credential verification and session issuance happen in `src/server/auth/`.

## Why

This keeps secrets and session lifecycle rules behind the API boundary.

## Consequences

- Browser auth changes must be checked against `docs/product-specs/login.md`.
- Server auth changes must be checked against `docs/design-docs/auth.md`.
- Agents should request a context packet for `auth` before changing files under `src/server/auth/`.
