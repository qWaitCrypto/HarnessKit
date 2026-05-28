# Web App With Auth Example

This example demonstrates HarnessKit's repo-native context packet workflow for a small web application auth boundary.

Try it with a built HarnessKit CLI:

```bash
harnesskit index examples/web-app-with-auth
harnesskit context auth examples/web-app-with-auth --json
harnesskit check examples/web-app-with-auth --json
```

Expected context shape:

- product spec: `docs/product-specs/login.md`
- design doc: `docs/design-docs/auth.md`
- decision record: `docs/decisions/0001-auth-boundary.md`
- code scope: `src/server/auth/`

The example intentionally contains only docs, not application source code. It shows how an agent should retrieve the smallest useful reading packet before editing auth-related files.
