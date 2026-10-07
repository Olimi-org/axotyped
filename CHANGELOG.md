## 0.4.0 (2026-10-06)

### Breaking changes

- the `ts-rs` feature is gone. Type export is now built in.
  - Migration: drop `"ts-rs"` from your feature list. Every type named in a route contract must implement `TS` (or mark the route `opaque()`).
  - The `*-impl` type-integration features stay optional and unchanged. `NoCollect` + monomorphization still keeps export code out of release binaries.
- every route goes through `register!()` + `#[endpoint]` (raw handlers no longer register).
  - Manual `.body()`/`.response()`/`.query()` overrides still work on registered routes.
- every extractor parameter of an `#[endpoint]` handler must declare whether it enforces authentication.
  - Axum's built-ins (`State`, `Json`, …) are covered by blanket impls.
  - Your own extractors get `#[axotyped::credential]` (rejects anonymous callers) or `#[axotyped::optional_credential]` (yields nothing for them). Custom extractors must be `pub`.
- private routes are now checked at compile time. Registering a handler that doesn't authenticate — and didn't declare `public`/`permissive` — fails at the registration call site.
  - `ApiRouter` carries its scope as a type parameter: `PrivateScope` by default, `PublicScope`/`PermissiveScope` inside groups, `LayeredAuthScope` after `auth_layer` (how you claim middleware coverage).
  - The reverse also fails: a `public`/`permissive` endpoint taking a credential-requiring extractor doesn't compile.
- `IntoApiRouter` is replaced by `IntoApiRouterAny`/`RouterParts`, and `ApiRouter::from_parts` is public. Group closures may now end in any scope.
- regenerate committed clients after upgrading. Declared flows add lifecycle machinery, an `AUTH_FLOW` constant, and an `auth` namespace; even flow-less output differs slightly (token-source preamble, reworded no-token errors).

### Features

- `auth_scheme!` declares your API's credential lifecycle: `kind: oauth2 | exchange | static_bearer`, `exchange`/`refresh`/`ws_ticket` route references, pinned `constant`s.
  - `build()` validates the declaration: references must resolve; `exchange`/`refresh` routes must not be `Private` (no bearer exists yet when they run); the `ws_ticket` mint must be `Private`; one flow per router tree (groups and `merge` adopt it or panic).
- declared Bearer flows generate session-lifecycle machinery: one `AuthStore` over `TokenStore`/`SessionStore`, single-flight refresh, 401 → refresh → one replay, proactive refresh from `expiresAt`, `onSessionExpired`, an `AUTH_FLOW` constant, and an `auth` namespace.
- `-> token` (exchange) and `-> { access, refresh, expires_in }` (oauth2, all but `access` optional) bind a default refresh hook to the declared route. Unusable bindings (body-taking, untyped, ws/redirect routes) warn and fall back to a manual `auth.refresh`.
- `GeneratorConfig::ws_ticket_path` is auto-derived from the flow's `ws_ticket` route when unset.
- `#[endpoint]` handlers returning `Response` skip response-type extraction (redirects, cookie callbacks); `RouteBuilder::opaque()` drops body/query/response metadata for non-contract endpoints (OAuth callbacks, webhooks).
- `WsRouteBuilder`/`RouteBuilder` forward `layer()`/`auth_layer()` mid-chain.
- `RouteCollection::extend` panics on two flows instead of silently keeping the first — same rule as the router's merge.

### Security hardening

- a forgotten identity extractor on a private route is now a compile error, and visibility–extractor contradictions fail at the endpoint definition.
- each credential claim sits on the type that enforces it, so searching for `#[credential]` finds every auth gate, and the declaration, client options, and generated code all read from the same flag.

## 0.3.0

### Breaking changes

- deny-by-default: routes require authentication unless declared public (`#[endpoint(public)]`, `[public]`, `group_public`); visibility is a three-state enum (`Private`/`Public`/`Permissive`), matched on directly instead of combining bools; the `[auth]` flag is gone
- removed `ApiRouter::auth_all()` / `RouteBuilder::auth()` / `WsRouteBuilder::auth()` — visibility is declared on the handler or scope
- `GeneratorConfig::default_credentials` defaults to `"same-origin"` (was `"include"`)
- generated output shape changed (encoded templates, quoted group keys, fully generator-controlled request options) — regenerate committed clients after upgrading

### Features

- `ApiRouter::layer()` — scope-scoped tower middleware, applied per-route, never leaked to parents/siblings
- `ApiRouter::auth_layer()` — like `layer()`, plus marks routes authenticated in metadata; contradictions with public declarations surface via `generate_with_warnings()`
- `RouteDefinition::declared` records declaration-site visibility independently of the effective flag
- new `Permissive` visibility (`#[endpoint(permissive)]`, `[permissive]`, `group_permissive`): credentials attached when available but never required — Bearer sends anonymously without a token, Cookie allows `omit`; stays credentialed for guard/cache/redirect purposes; the flag is generator-pinned per route (not caller-suppliable)
- `GeneratorConfig::auth_scheme` — `Bearer` (default, `getToken` + `Authorization` header), `Cookie` (session cookies, `"omit"` refusal, native WS cookies, optional CSRF via `csrf_header_name`), `None`
- `GeneratorConfig::large_int_type` (`number`/`bigint`/`string`) applied to client signatures and `ts_config()`/`export_types_with()` bindings alike
- `GeneratorConfig::ws_ticket_path` — ticket-handshake WS client for `[ws][auth]` under Bearer
- `generate_with_warnings()` diagnostics: public-behind-auth-layer contradictions, `None`-scheme auth routes, missing WS credential pathway
- `allow_redirects` route property (`#[endpoint(public, allow_redirects)]`, `.allow_redirects()`): lets credentialless calls follow redirects; route methods take no caller options — redirect and auth-softening behavior is generator-pinned, never runtime-flippable
- `RouteTable::collect_types()` / `build()` work without the `ts-rs` feature

### Security hardening

- transport guard: credentials (auth or cookies) only ride https/loopback; `allowInsecureHttp` permits the connection but never credentialed non-loopback traffic
- authenticated requests send `cache: "no-store"` and refuse redirects; only credentialless calls to `[allow_redirects]` routes follow them
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
