# axotyped

Auto-generate typed TypeScript API clients from Axum route metadata.

## Quick start

Annotate your handlers with `#[endpoint]`, register them with `register!()`, and get both an Axum router and a typed TypeScript client:

```rust
use axotyped::{ApiRouter, endpoint, register};

// Derive axotyped::TS (re-exported ts-rs) for any types crossing the wire
#[derive(serde::Serialize, axotyped::TS)]
pub struct ProjectResponse {
    pub id: String,
    pub title: String,
}

// #[endpoint] extracts Json<T>, Query<T>, and response types from the signature
#[endpoint]
pub async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<Vec<ProjectResponse>>, StatusCode> {
    // ...
}

#[endpoint]
pub async fn create_project(
    State(state): State<AppState>,
    Json(body): Json<CreateProjectRequest>,
) -> Result<Json<ProjectResponse>, StatusCode> {
    // ...
}

// define_routes! builds both the server router and TypeScript metadata
define_routes! {
    pub Routes for AppState, |r| {
        r.group("projects", |g| {
            // For a auto-prefixing option consider group_prefixed
            g.get("/projects", register!(list_projects))
             .post("/projects", register!(create_project))
        })
    }
}

// Server startup:
let app = Routes::router().with_state(state);
```

Generates a TypeScript client:

```typescript
const api = createApiClient({
  baseUrl: "http://localhost:3000",
  getToken: async () => localStorage.getItem("token"),
});

const projects = await api.projects.listProjects();
const newProject = await api.projects.createProject({ title: "My Project" });
```

Handler names auto-convert to camelCase: `list_projects` → `listProjects`, `create_project` → `createProject`. Override with `.as_("customName")` when needed.

## Installation

```toml
[dependencies]
axotyped = "0.4"
```

TypeScript type export is built in — every type named in a route contract derives `axotyped::TS`. Optional `*-impl` features add TS support for specific types (`chrono-impl`, `uuid-impl`, `serde-json-impl`, …); `serde-compat`, `no-serde-warnings`, and `format` adjust the derive and output. `axum-ws` enables WebSocket handlers.

`axotyped` re-exports its TS machinery as `axotyped::TS`, so no separate type-export dependency is needed.

## How it works

**Two crates work together:**

| Crate | Generates | Purpose |
|---|---|---|
| **`axotyped`** | `generated.ts` — typed fetch wrappers | Route definitions, client factory, error handling |
| **[`ts-rs`](https://crates.io/crates/ts-rs)** | Individual `.ts` type files | TypeScript interfaces for your Rust structs |

`axotyped` generates `import type { ProjectResponse } from "./bindings/ProjectResponse"` — those files come from `ts-rs`.

## Defining routes

### Recommended: `#[endpoint]` + `register!()`

Annotate handlers with `#[endpoint]` and pass them to `register!()` inside the builder. Types are inferred from the function signature — no manual specification needed.

```rust
use axotyped::{ApiRouter, endpoint, register};

#[derive(serde::Deserialize, axotyped::TS)]
pub struct CreateProjectRequest {
    pub title: String,
}

#[endpoint]
pub async fn create_project(
    State(state): State<Arc<AppState>>,
    Json(body): Json<CreateProjectRequest>,
) -> Result<Json<ProjectResponse>, StatusCode> {
    // ...
}

#[endpoint]
pub async fn list_projects(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<ProjectResponse>>, StatusCode> {
    // ...
}

#[endpoint]
pub async fn delete_project(
    Path(key): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<StatusCode, StatusCode> {
    // ...
}

ApiRouter::<Arc<AppState>>::new()
    .group_prefixed("admin", |g| {
        g.post("/project", register!(create_project))
             .done()
         .get("/project", register!(list_projects))
             .done()
         .delete("/project/{key}", register!(delete_project))
             .done()
    })
    .build()
```

**How type inference works:**

| Signature pattern | Extracted type |
|---|---|
| `Json(body): Json<T>` in params | Body type: `T` |
| `Query(params): Query<T>` in params | Query type: `T` |
| `-> Result<Json<T>, E>` | Response type: `T` |
| `-> Result<StatusCode, StatusCode>` | No response (void) |
| `-> Json<T>` (non-Result) | Response type: `T` |

Inner types of `Vec<T>` and `Option<T>` are automatically collected for TypeScript export, so `Vec<ProjectTag>` correctly generates `ProjectTag.ts`.

`impl Trait` types (e.g. `Result<impl IntoResponse, E>`) are gracefully skipped — no metadata is generated, compilation is not affected.

### Grouping routes

**`.group(name, closure)`** — sets the TypeScript namespace for routes inside the closure (no URL path prefix):

```rust
ApiRouter::<AppState>::new()
    .group("auth", |g| {
        g.post("/login", register!(login)).done()
         .post("/register", register!(register_user)).done()
    })
    .build()
// Generates api.auth.login() and api.auth.registerUser() targeting /login and /register
```

**`.group_prefixed(name, closure)`** — closure-based grouping with scoped URL path prefix (`/{name}`) and TypeScript namespace. The group's config does not leak to routes registered after the closure.

### Authentication model: deny-by-default, checked by the compiler

Every route is private unless you say otherwise. An unannotated route can never generate a credential-less client. Since 0.4.0 this is a compile error, reported where the mistake is.

It works in two steps. First, tell axotyped which of your extractors actually enforce authentication. Axum's built-ins (`State`, `Json`, `Query`, `Path`, …) are already covered — they never reject anyone. Your own identity extractors pick a side:

```rust
// Rejects anonymous callers with 401/403:
#[axotyped::credential]
pub struct AuthUser(pub UserIdentity);
// impl FromRequestParts for AuthUser ...

// Returns None for anonymous callers instead of rejecting them:
#[axotyped::optional_credential]
pub struct OptionalAuthUser(pub Option<UserIdentity>);
```

Two rules to remember: custom extractors must be `pub` (their credential flag lands in a public type, so a private struct won't compile), and every parameter of an `#[endpoint]` handler must resolve to one of these — a missing impl is a compile error telling you to classify it.

Second, every private route must take an extractor that enforces authentication:

```rust
// Private by default — compiles because AuthUser rejects anonymous callers:
#[endpoint]
async fn reset(AuthUser(_u): AuthUser) -> StatusCode { /* ... */ }
r.group_prefixed("admin", |g| g.post("/reset", register!(reset)).done())

// Public route — no authentication:
#[endpoint(public)]
async fn health() -> StatusCode { StatusCode::OK }

// Permissive route — the handler runs either way; OptionalAuthUser is
// Some when the caller is logged in, None when anonymous:
#[endpoint(permissive)]
async fn get_feed(user: OptionalAuthUser) -> Json<Feed> { /* ... */ }

// Or opt out per scope (e.g. webhooks):
r.group_public("webhooks", |g| {
    g.set_prefix("/webhooks").post("/stripe", register!(stripe_hook)).done()
})
```

Two mistakes become compile errors:

* Private route without an enforcing extractor → error at the **registration call** (`PrivateScope: AllowsRoute … not satisfied`). You forgot the extractor.
* `public`/`permissive` handler that *takes* a credential extractor → error at the **endpoint definition** (`AssertCredentiallessDeclaration<false>`). Anonymous callers were promised a route that rejects them; either make the route private or drop the enforcing extractor.

Middleware counts too: `.auth_layer(mw)` says "this middleware enforces auth for everything under it" and lifts the extractor requirement for that scope (claim it explicitly there). Plain `.layer()` doesn't — use it for non-auth middleware like signature checks or rate limiting.

A `permissive` route sends the token when the client has one and sends nothing when it doesn't; the handler runs in both cases (Bearer: the header is attached only when a token resolves; Cookie: `omit` is allowed). Permissive routes still count as credentialed for guard/cache/redirect purposes, and degrade to anonymous under `AuthScheme::None` without diagnostics.

Redirect-following is a server-side route property (`#[endpoint(public, allow_redirects)]`, `.allow_redirects()`), never a caller option: only credentialless calls to declaring routes follow redirects, everything else refuses. Route methods take no options argument.

The prefix defaults to `"/{name}"` but can be overridden with `.set_prefix()` inside the closure.

```rust
ApiRouter::<Arc<AppState>>::new()
    .group_prefixed("admin", |g| {
        g.post("/project", register!(create_project))
        .get("/project", register!(list_projects))
        .delete("/project/{key}", register!(delete_project))
    })
    .get("/health", register!(health))
    .build()
```

### Manual type specification

You can still specify types explicitly on the builder when needed. This works on registered routes (raw handlers no longer compile):

```rust
ApiRouter::<AppState>::new()
    .get("/projects", register!(list_projects))
        .response::<Vec<ProjectResponse>>()
    .post("/projects", register!(create_project))
        .json::<CreateProjectRequest, ProjectResponse>()
    .build()
```

## Route Table Patterns & Zero-Cost Production Builds

`axotyped` uses generic monomorphization (`ApiRouter<S, C>`) so that TypeScript export code is **dead-code-eliminated (DCE) from production release builds**.

### 1. Macro (`define_routes!`)

Generates a struct implementing `RouteTable` with `.router()` and `.collect_types()` methods:

```rust
use axotyped::{define_routes, register};

define_routes! {
    pub Routes for Arc<AppState>, |r| {
        r.get("/health", register!(health))
         .group_prefixed("admin", |g| {
             g.post("/project", register!(create_project))
         })
    }
}

// Server startup (lean — no type-export code in release binary):
let router = Routes::router().with_state(state);

// Type collection (debug/tests):
let collection = Routes::collect_types();
```

### 2. Trait (`RouteTable` — Macro-Free)

Implement `RouteTable` directly for macro-free route definitions with full IDE support:

```rust
use axotyped::{ApiRouter, Collector, RouteTable, register};

pub struct Routes;

impl RouteTable<Arc<AppState>> for Routes {
    fn define<C: Collector>(
        r: ApiRouter<Arc<AppState>, C>,
    ) -> ApiRouter<Arc<AppState>, C> {
        r.get("/health", register!(health))
         .group_prefixed("admin", |g| {
             g.post("/project", register!(create_project))
         })
    }
}

let router = Routes::router().with_state(state);
let collection = Routes::collect_types();
```

### 3. Generic Function

Pass a generic function into `build_routes` and `collect_routes`:

```rust
use axotyped::{ApiRouter, Collector, build_routes, collect_routes, register};

fn routes<S, C: Collector>(r: ApiRouter<S, C>) -> ApiRouter<S, C> {
    r.get("/health", register!(health))
     .group_prefixed("admin", |g| {
         g.post("/project", register!(create_project))
     })
}

let router = build_routes(routes).with_state(state);
let collection = collect_routes(routes);
```

### 4. Direct Builder

Construct `ApiRouter` directly without structs or functions:

```rust
use axotyped::{ApiRouter, TypedApiRouter, register};

// Server startup (NoCollect):
let (router, _) = ApiRouter::<Arc<AppState>>::new()
    .get("/health", register!(health))
    .build();

// Type collection (TypeRegistry):
let (_, collection) = TypedApiRouter::<Arc<AppState>>::new()
    .get("/health", register!(health))
    .build();
```

## Generating the client

Generation needs your compiled route functions, so we need to configure the setup to run after compile-time.

**There are two common patterns:**

**From `main.rs`** — gated behind a CLI flag for debug builds:

```rust
// In main.rs, debug builds only:
#[cfg(debug_assertions)]
if args.generate_bindings {
    let routes = routes::route_collection();
    routes.export_types(&bindings_dir).unwrap();
    axotyped::generate_to_file(&routes, &config()).unwrap();
}
```

**From a `#[test]`** — regenerate on demand:

```rust
#[test]
fn generate_ts_client() {
    let routes = my_app::routes();
    routes.export_types(std::path::Path::new("./bindings")).unwrap();
    axotyped::generate_to_file(&routes, &config()).unwrap();
}

// CI check — fails if the committed file is stale:
#[test]
fn check_ts_client() {
    let routes = my_app::routes();
    axotyped::check(&routes, &config())
        .expect("Generated TypeScript client is out of date!");
}
```

Local: `cargo test generate_ts_client` to regenerate.
CI: `cargo test check_ts_client` fails if the committed file is stale.

## GeneratorConfig

| Field | Default | Description |
|---|---|---|
| `bindings_dir` | `"./bindings"` | Where `ts-rs` writes type files |
| `output_path` | `"./generated.ts"` | Where to write the generated client |
| `factory_name` | `"createApiClient"` | Name of the factory function |
| `enable_groups` | `true` | Nest routes into group objects |
| `error_class_name` | `"ApiError"` | Name of the generated error class |
| `options_interface_name` | `"ClientOptions"` | Name of the options interface |
| `default_credentials` | `"same-origin"` | Default `RequestCredentials` value |
| `type_import_prefix` | (computed) | Import path from generated file to bindings dir |
| `format_command` | `None` | Shell command to format after generation |
| `large_int_type` | `"number"` | TS binding for u64/i64/u128/i128/usize/isize (`"number"`, `"bigint"`, or `"string"`) |
| `auth_scheme` | `Bearer` | Client authentication model: `Bearer` (token header), `Cookie` (session cookies), or `None` |
| `csrf_header_name` | `None` | Anti-CSRF header attached to mutating requests under `AuthScheme::Cookie` (e.g. `Some("X-CSRF-Token")`) |
| `ws_ticket_path` | `None` | Endpoint issuing single-use WS tickets for authenticated WebSocket routes under `AuthScheme::Bearer` |

### Auth schemes

- **`AuthScheme::Bearer`** — authenticated routes resolve a token via
  `ClientOptions.getToken` and send `Authorization: Bearer ...`. Requests fail
  closed: no configured source or no token aborts the call instead of sending
  an unauthenticated request. `Permissive` routes attach the token when one
  resolves and otherwise send anonymously.
- **`AuthScheme::Cookie`** — session cookies ride automatically. The client
  refuses `"omit"` credentials on authenticated routes, and with
  `csrf_header_name` set it attaches the anti-CSRF proof on POST/PUT/PATCH/DELETE
  via `ClientOptions.csrfToken`. Cross-origin `baseUrl` requires
  `credentials: "include"` (plus compatible CORS and `SameSite=None; Secure`
  cookies) — `"same-origin"` never sends cookies cross-origin.
- **`AuthScheme::None`** — no auth machinery is generated. Authenticated
  routes produce diagnostics from `generate_with_warnings()`, since their
  requests will carry no credentials.

### Server-derived auth metadata

Routes are private by default, but the strongest way to declare authentication
is to enforce it: `.auth_layer(middleware)` applies auth middleware with the same
scoping rules as `.layer()` **and** marks every route under it as authenticated
in the generated client. The client flag is derived from server middleware, so
the two cannot drift:

```rust,ignore
r.group_prefixed("admin", |g| {
    g.auth_layer(axum::middleware::from_fn_with_state(state, require_admin))
        .get("/course", register!(list_courses)) // client method requires credentials
})
```

A route declared public (`group_public`, `#[endpoint(public)]`) that ends up
behind an `.auth_layer()` stays authenticated in metadata and produces a
diagnostic — if the middleware isn't user authentication (e.g. webhook
signature checks), use plain `.layer()` instead.

### Credential lifecycle: `auth_scheme!`

Auth gates control which callers reach each route. `auth_scheme!` declares where the bearer token itself comes from — which routes mint and renew it — so the generated client can handle session renewal instead of every consumer reimplementing 401-retry by hand. Declare it once per API:

```rust,ignore
// The bootstrap: session cookie in, fresh JWT out. Anonymous callers get
// a normal response rather than a 401, so the handler is permissive and
// the token field is Option<String> (None means the session is unrecoverable).
#[derive(serde::Serialize, axotyped::TS)]
pub struct MeResponse {
    pub authenticated: bool,
    pub token: Option<String>,
}

#[axotyped::endpoint(permissive)]
pub async fn me(user: OptionalAuthUser) -> Json<MeResponse> { /* ... */ }

axotyped::auth_scheme! {
    pub SessionAuth {
        kind: exchange,
        exchange: register!(me),
        refresh:   register!(me) -> token,
    }
}

ApiRouter::<Arc<AppState>>::new()
    .auth_flow(SessionAuth::flow())
    .get("/auth/me", register!(me))
    .build()
```

Pick the `kind` that matches your app:

| `kind` | Your setup | What you declare |
|---|---|---|
| `exchange` | Cookie/session → short-lived JWT (the common case) | `exchange` (+ usually `refresh` on the same route) and `-> token` |
| `oauth2` | External issuer via backend proxy routes | `exchange`/`refresh` and `-> { access, refresh, expires_in }` (`refresh`/`expires_in` optional — absent refresh keeps the stored one, absent `expires_in` falls back to the JWT `exp` claim) |
| `static_bearer` | API keys / test tokens with no renewal | Only `ws_ticket` and `constant`s (see below) |

A few things worth knowing:

* **`exchange` vs `refresh`.** `exchange` is the first mint (login, cookie swap); `refresh` is the renewal the client calls on 401. They can be the same route (cookie re-exchange, as above) or different ones. You can list several `exchange` routes; there is at most one `refresh`.
* **`-> token` binds the default refresh hook.** With the field declared, the client renews through your typed response (`resp?.token ?? null`) and persists the result to the store, so the 401 replay re-reads a fresh bearer. Pass `auth.refresh` only to customize. Field names are camelCased for the client (`access_token` → `accessToken`).
* **`build()` rejects bad declarations at startup.** Referenced routes must exist (renamed with `.as_()`? reference the client name via `bootstrap_named`/`refresh_named`/`ws_ticket_named`); `exchange`/`refresh` routes must *not* be `Private` (no bearer exists yet when they run, so requiring one would be circular); the `ws_ticket` mint *must* be `Private` (it converts a live bearer into a single-use proof); token fields must match the kind; one flow per router tree (groups and `merge` adopt it or panic). Anything the hook can't bind to (body-taking, untyped, ws/redirect routes) produces a `generate_with_warnings()` warning and falls back to a manual `auth.refresh`.
* **The client does the rest.** With a Bearer flow declared you get: an `AuthStore` (`TokenStore` for strings, `SessionStore` for OAuth pairs — implement whichever matches your kind), single-flight refresh (parallel 401s share one call), 401 → refresh → one replay on credentialed routes, proactive refresh when `expiresAt` nears, `onSessionExpired` for terminal loss, a pinned `AUTH_FLOW` constant, and an `auth` namespace aliasing the lifecycle routes (`api.auth.me()` next to `api.me()`):

```typescript
const api = createApiClient({
  baseUrl: "http://localhost:3000",
  auth: {
    store, // your TokenStore / SessionStore
    onSessionExpired: () => redirectToLogin(),
    // refresh: custom fn — only if you skipped `-> token`
  },
});
```

* **`static_bearer` stays lean.** It emits only the `AUTH_FLOW` facts, the `auth` namespace, and the WS ticket path below.
* **Extras.** `constant issuer: "https://…"` pins facts like issuer/client-id into `AUTH_FLOW.constants` (key camelCased). `ws_ticket: register!(issue_ticket)` names the mint for the WS handshake, and `GeneratorConfig::ws_ticket_path` is auto-derived from it when unset.

## Generated output

The generated client includes:
- Type imports from `ts-rs` bindings
- A typed error class (extends `Error` with `status` and `body`)
- A client options interface shaped by the auth scheme (`baseUrl`,
  scheme-specific credential fields, `credentials`, `fetch`, `onError`)
- A factory function returning typed fetch methods with optional grouping
- A type alias: `export type ApiClient = ReturnType<typeof createApiClient>`

See [tests/snapshots/yauth_style.ts](tests/snapshots/yauth_style.ts) for a full example.

## License

MIT
