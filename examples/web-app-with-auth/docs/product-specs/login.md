---
status: active
authority: canonical
scope_paths:
  - src/web/login.tsx
  - src/server/auth/
---

# Login Product Spec

Users sign in with email and password and receive a server-managed session.

## Requirements

- The browser submits credentials through `src/web/login.tsx`.
- The server validates credentials inside `src/server/auth/`.
- Failed login attempts return a generic error.
- Successful login creates an HTTP-only session cookie.

## Related Docs

- `docs/design-docs/auth.md`
- `docs/decisions/0001-auth-boundary.md`
