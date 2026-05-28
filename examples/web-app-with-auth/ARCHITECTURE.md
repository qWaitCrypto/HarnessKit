---
status: active
authority: canonical
---

# Architecture

Small web application with a browser client, an API server, and a database-backed authentication boundary.

## Boundaries

- `src/web/` owns browser routes and login form behavior.
- `src/server/auth/` owns session creation, password verification, and auth middleware.
- `src/server/db/` owns persistence.

## Reading Path

For authentication work, start with `docs/product-specs/login.md`, then read `docs/design-docs/auth.md` and `docs/decisions/0001-auth-boundary.md`.
