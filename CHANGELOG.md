## 0.3.0

### Breaking changes

- deny-by-default: routes require authentication unless declared public (`#[endpoint(public)]`, `[public]`, `group_public`); the `[auth]` flag is gone
- removed `ApiRouter::auth_all()` / `RouteBuilder::auth()` / `WsRouteBuilder::auth()` — visibility is declared on the handler or scope
- `GeneratorConfig::default_credentials` defaults to `"same-origin"` (was `"include"`)
- generated output shape changed (encoded templates, quoted group keys, per-call `opts`, enforced `method`) — regenerate committed clients after upgrading

### Features

- `ApiRouter::layer()` — scope-scoped tower middleware, applied per-route, never leaked to parents/siblings
- `ApiRouter::auth_layer()` — like `layer()`, plus marks routes authenticated in metadata; contradictions with public declarations surface via `generate_with_warnings()`
- `RouteDefinition::declared_public` records declaration-site visibility independently of the effective flag
- `GeneratorConfig::auth_scheme` — `Bearer` (default, `getToken` + `Authorization` header), `Cookie` (session cookies, `"omit"` refusal, native WS cookies, optional CSRF via `csrf_header_name`), `None`
- `GeneratorConfig::large_int_type` (`number`/`bigint`/`string`) applied to client signatures and `ts_config()`/`export_types_with()` bindings alike
- `GeneratorConfig::ws_ticket_path` — ticket-handshake WS client for `[ws][auth]` under Bearer
- `generate_with_warnings()` diagnostics: public-behind-auth-layer contradictions, `None`-scheme auth routes, missing WS credential pathway
- per-call `opts?: RequestOptions` on every route method (`allowRedirects` opt-out for credentialless public calls)
- `RouteTable::collect_types()` / `build()` work without the `ts-rs` feature

### Security hardening

- transport guard: credentials (auth or cookies) only ride https/loopback; `allowInsecureHttp` permits the connection but never credentialed non-loopback traffic
- authenticated requests send `cache: "no-store"` and refuse redirects; only credentialless public calls with `allowRedirects: true` follow them
- Cookie `[ws][auth]` refuses `credentials: "omit"` before upgrading
- path literals escaped for their JS string context; group/method names emitted as quoted, escaped keys
- path params validated (`is_valid_js_identifier`, synthetics `__param_N`), always `encodeURIComponent`'d, extracted from the resolved `full_path`
- falsy bodies serialized (`body !== undefined ? ...`); `u128`/`i128` treated as primitives (no broken imports)

### Fixes

- routes factory receives client `options` (redirect/WS methods previously referenced it out of scope)
- `default_credentials` / `csrf_header_name` values escaped in emitted string literals
- `check()` temp files use `create_new(true)` + monotonic-counter fallback

## 0.2.0 (2026-06-08)

### Breaking changes

- `axum` is now a required dependency — the `axum` feature flag is removed
- `api_routes!` macro is now internal-only, not part of the public API

### Features

- collect inner types of `Vec<T>` / `Option<T>` in `#[endpoint]` macro, fixing missing `.ts` files for types that only appear inside containers
- add `group_with()` closure-based grouping with scoped URL prefix, default auth, and TS namespace
- re-export `ts-rs` as `axotyped::TS` so consuming crates don't need a separate `ts-rs` dependency
- rebrand from `axfetchum` to `axotyped`
- use forked `ts-rs` dependency pinned to commit with companion-crate support

## 0.1.4 (2026-03-30)

### Fixes

- replace remaining axum-ts-client references with axfetchum
- apply cargo fmt formatting fixes

## 0.1.3 (2026-03-24)

### Features

- switch to crates.io trusted publishing, remove CARGO_REGISTRY_TOKEN

### Fixes

- update crate name references from axum_ts_client to axfetchum
- use dot notation for Authorization header (biome useLiteralKeys)

## 0.1.2 (2026-03-17)

### Features

- switch to crates.io trusted publishing, remove CARGO_REGISTRY_TOKEN

### Fixes

- update crate name references from axum_ts_client to axfetchum

## 0.1.1 (2026-03-17)

### Fixes

- update crate name references from axum_ts_client to axfetchum

## 0.1.7 (2026-03-16)

### Features

- add ApiRouter builder and Vec<T>/Option<T> macro support

## 0.1.6 (2026-03-15)

### Fixes

- resolve fetch at call time for OTel instrumentation compatibility

## 0.1.5 (2026-03-08)

### Fixes

- surface release failures and include refactor commits
- correct knope extra_changelog_sections format

## 0.1.4 (2026-02-14)

### Fixes

- publish crate before git push to prevent cancellation
- add Cargo.lock to versioned_files and --allow-dirty

## 0.1.3 (2026-02-14)

### Features

- add format_command to GeneratorConfig

## 0.1.2 (2026-02-14)

### Fixes

- add CARGO_REGISTRY_GLOBAL_CREDENTIAL_PROVIDERS for cargo publish

## 0.1.1 (2026-02-14)

### Features

- initial axum-ts-client crate
- add automated semver releases via knope + Forgejo CI

### Fixes

- use --strip-components=1 for knope tarball extraction
