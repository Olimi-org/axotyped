## 0.4.0 (2026-10-06)

### Breaking changes

- **The client runtime no longer owns credential state.** The `AuthEngine`, the provenance ledger, the store mirrors (`TokenStore`/`SessionStore`/`AuthStore`/`OAuthSession`), `AuthLifecycleOptions`, and the `auth: { store, refresh, onSessionExpired }` options object are gone. `createTransport` takes `(options, policy, operations)`.
- **Every client names its credential with `getToken`.** A declared flow offers `onUnauthorized` alongside it; there is no second bearer source.
- **`onUnauthorized` replaces the engine's 401 recovery.** Called after a 401 on a `required` bearer route; return the credential to replay with, or `null` when the session cannot be renewed. Throwing is treated as `null` — either way the server's own 401 surfaces, not the hook's error. Replay is bounded at two, and the replay credential is the hook's *return value*: the transport never re-reads `getToken` to decide who the caller is, so a request cannot be re-sent under a session the application has since replaced.
- **Cookie routes do not replay.** Nothing an application does mid-flight changes what the browser already put in the cookie jar; a cookie 401 is terminal by construction.
- **Proactive renewal, rotation persistence, and single-flight are the application's.** `singleFlight` and `jwtExpiry` are exported as stateless primitives for it.
- **`-> token` now buys a generated renewal helper instead of wiring the transport.** `auth_scheme!` emits `refreshBearer(client)` (or `refreshSession(client)` for an oauth2 pair) that calls the declared route and lifts the declared fields off its response. Because the helper reads them off the *imported response type*, `tsc` checks every declared name against what the client will parse — replacing the runtime's uncheckable string-keyed extraction. `AUTH` is emitted as data for application glue; nothing in the runtime interprets it.
- **A route or group named after a built-in client helper is reported.** `Object.assign` would overwrite the generated property, and calling it would derive a new client instead of sending the request.
- Type export is built in; the `ts-rs` feature is gone. Drop `"ts-rs"` from features. Every type in a route contract implements `TS` (or mark the route `opaque()`).
- Every route uses `register!()` + `#[endpoint]`. Manual `.body()`/`.response()`/`.query()` overrides still apply to registered routes.
- Every `#[endpoint]` extractor parameter declares authentication. Axum built-ins are covered by blanket impls. Custom extractors use `#[axotyped::credential]` or `#[axotyped::optional_credential]` and must be `pub`.
- Private routes are checked at compile time. A handler without an enforcing extractor fails at the registration call. `ApiRouter` carries its scope as a type parameter (`PrivateScope` default, `PublicScope`/`PermissiveScope` in groups, `LayeredAuthScope` after `auth_layer`).
- `public`/`permissive` endpoints taking a credential-requiring extractor do not compile. Credential-requiring handlers in `group_public`/`group_permissive` scopes do not compile.
- The credential fold matches extractor paths. A qualified non-`axum` path folds; a bare name matching an axum built-in is treated as axum's (qualify custom extractors with colliding names). `public`/`permissive` endpoints check ambient-classified parameters at the type level (`EndpointParam<Auth = OptionalCredential>`). `ConnectInfo` covers any address type.
- `ApiRouter::new`, `Default`, and `from_parts` accept initial scopes only. Enter `LayeredAuthScope` via `.auth_layer(...)`.
- `IntoApiRouter` is replaced by `IntoApiRouterAny`/`RouterParts`; `ApiRouter::from_parts` is public. Group closures may end in any scope.
- Generation writes two files: the client and a sibling `<name>-runtime.ts`. The client asserts the runtime spec version at import.
- `[ws]` routes declare event types (`.events::<Send, Receive>()` on the builder; `send:`/`receive:` in macro form). Hand-built definitions without both types generate no client method and warn.
- WebSocket route methods return `Promise<TypedWebSocket<S, R>>`.
- `ApiError` is defined by the shared runtime and re-exported by the client.
- `api_routes!` is removed from the library. `GeneratorConfig::ws_ticket_path`/`ws_ticket_method` are removed; declare `ws_ticket: register!(…)` in `auth_scheme!`.
- Every client exposes `getToken` (sync or async forms both accepted); a declared flow adds `onUnauthorized` beside it. There is no `auth` options object.
- `TransportOptions.csrfToken` accepts sync and async forms.

### Features

- `auth_scheme!` declares the credential lifecycle: `kind: oauth2 | exchange | static_bearer`, `exchange`/`refresh`/`ws_ticket` references, `constant`s. `build()` validates references, visibility rules (`exchange`/`refresh` not `Private`; `ws_ticket` `Private`), and one flow per router tree.
- The client file carries declarations (`OPERATIONS` table, `AUTH` strategy, methods, options, factory). The runtime module carries the transport — executor, guards, replay policy, observability — and the WS handshake. It holds no credential state.
- `AUTH` carries kind, refresh op, verbatim field names, `skewMs`, bootstrap/wsTicket/constants. `TRANSPORT` carries scheme and WS ticket wiring.
- `-> token` (exchange) and `-> { access, refresh, expires_in }` (oauth2 members optional) generate a typed renewal helper — `refreshBearer(client)` / `refreshSession(client)` — that calls the declared route and lifts the fields off its response; `tsc` checks every declared name against the imported response type. Declared fields are also checked against `WireFields` wire names at the declaration site.
- Routes the renewal helper cannot call (missing route, body/path/query inputs, untyped, ws/redirect) generate no helper and warn; renewal is whatever the application's `onUnauthorized` does.
- `#[endpoint]` handlers returning `Response` skip response extraction. `RouteBuilder::opaque()` drops body/query/response metadata.
- `WsRouteBuilder`/`RouteBuilder` forward `layer()`/`auth_layer()` mid-chain.
- `RouteCollection::extend` adopts a flow when absent, panics on two flows, and re-validates the surviving flow against the merged table.
- Lifecycle references must resolve to exactly one route; ambiguous names fail `build()`.
- Duplicate client method names within one group fail `build()`. `OPERATIONS` keys are group-qualified. A root-level route named `auth` warns and skips the synthetic `api.auth.*` alias.
- 401 replay fires on status 401 only, on `required` routes under the bearer scheme, bounded at two replays. The replay credential is `onUnauthorized`'s return value — the transport never re-reads `getToken` mid-request, so a request cannot be re-sent under a session the application has since replaced; a hook returning the just-rejected credential does not re-send (the server's 401 surfaces). Cookie routes never replay.
- `refreshSession` normalizes `expires_in` through `Number()`. Token field names are emitted verbatim. `AUTH.constants` keys are quoted and escaped.
- `withOptions` derives a client over the same options object — `getToken` and `onUnauthorized` are the application's own closures, so derived clients share the credential source by construction.
- `onRequest`/`onResponse` options receive the operation descriptor. `onResponse` receives a `response.clone()`. Throwing observers do not fail the call.
- `Content-Type: application/json` is sent on body-carrying calls only. Credentialed requests send `cache: "no-store"`. Query arrays serialize as repeated keys for HTTP and WS.
- `TypedWebSocket.close(code?, reason?)` forwards to the socket.
- `compute_import_prefix` handles deeper output paths. `contains_null_type` strips outer parens. oauth2 flows without declared fields emit a field-less strategy and no renewal helper — renewal is the application's `onUnauthorized`.
- Behavioral suite runs the runtime under `bun test` (skips when bun is absent); a second spec runs a generated client end to end.

### Security hardening

- Private routes without an enforcing extractor do not compile; visibility–extractor contradictions fail at the endpoint definition.
- Credential claims sit on the enforcing types; declaration, client options, and generated code read the same flag.

### Fixes

- `EndpointParam` impls cover the axum extractors handlers actually take: `String` (raw text body), `Form<T>`, `http::Method`/`Uri`/`Version`, `Body`, and `NestedPath` join the blanket set, and `axum::extract::Multipart` sits behind the new `axum-multipart` feature. Without these, `#[endpoint]`'s per-parameter assertion plus the orphan rule made a form, multipart, or plain-text handler impossible to register — a local newtype was the only way around. (No `Result<T, E>` impl: axum 0.8 has no Result extractor, so the shape cannot appear in a signature.)
- the renewal helper brackets non-identifier names (`client["email-password"]["x-refresh"]()`): it built the call with the object-literal key emitter, so a hyphenated group or `.as_()`-renamed lifecycle route produced `client."email-password".exchange()` — not valid TypeScript, the generated file did not compile.
- the renewal helper calls the route's top-level method when grouping is disabled: with `enable_groups: false` the flat emitter puts every method at the root, so a grouped lifecycle route made `refreshBearer` reference `client.auth.me()` — a property the flat client does not have (tsc failure, runtime TypeError). The `withOptions` collision check follows the same layout: flat mode flags a grouped route named after a helper (its method is top-level) and stops flagging a group name that creates no property; the warning names the property that actually collides.
- `TokenFields` is re-exported: `AuthFlow::token_fields` is public, but its type was unnameable downstream.
- the `params` module docs no longer show a non-compiling empty `impl EndpointParam` (stable Rust has no associated-type defaults) or mention a `MaybeAllowed` bound that does not exist; the README renewal example no longer hands `onUnauthorized` a proactive `renew` that can return a revoked-but-unexpired token (the transport does not re-send a rejected credential, so that 401 was terminal) — forced renewal (`renewNow`) is split from the proactive check, and the snippet's imports are real.

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
