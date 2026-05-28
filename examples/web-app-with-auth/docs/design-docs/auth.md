---
status: active
authority: canonical
scope_paths:
  - src/server/auth/
supersedes:
  - docs/decisions/0001-auth-boundary.md
---

# Auth Design

Authentication is handled by the API server. The browser receives only an HTTP-only session cookie.

## Design

- `src/server/auth/credentials.ts` verifies credentials.
- `src/server/auth/session.ts` creates and validates server-side sessions.
- `src/server/auth/middleware.ts` protects authenticated routes.

## Constraints

- Do not put password verification in browser code.
- Do not expose raw session tokens to JavaScript.
- Keep the auth boundary aligned with `docs/decisions/0001-auth-boundary.md`.

## Related Docs

- `docs/product-specs/login.md`
- `docs/decisions/0001-auth-boundary.md`
