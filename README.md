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
axotyped = { version = "0.2", features = ["ts-rs"] }
```

- **`ts-rs`** — enables TypeScript type export for your structs/enums (recommended)

Without `ts-rs`, type collection is a no-op — routes are still registered but no `.ts` type files are generated.

`axotyped` re-exports `ts-rs` as `axotyped::TS`, so you don't need a separate `ts-rs` dependency.

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

### Authentication model: deny-by-default

Every route is treated as requiring authentication unless explicitly declared public. An unannotated route can never generate a credential-less client, so "forgot the auth annotation" failures are structurally impossible.

```rust
// Private by default — no annotations needed:
r.group_prefixed("admin", |g| g.post("/reset", register!(reset)).done())

// Opt out per handler:
#[endpoint(public)]
async fn health() -> StatusCode { StatusCode::OK }

// Or opt out per scope (e.g. webhooks):
r.group_public("webhooks", |g| {
    g.set_prefix("/webhooks").post("/stripe", register!(stripe_hook)).done()
})
```

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

You can still specify types explicitly on the builder when needed:

```rust
ApiRouter::<AppState>::new()
    .get("/projects", list_projects)
        .response::<Vec<ProjectResponse>>()
    .post("/projects", create_project)
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

// Server startup (lean — no ts-rs code in release binary):
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
  an unauthenticated request.
- **`AuthScheme::Cookie`** — session cookies ride automatically. The client
  refuses `"omit"` credentials on authenticated routes, and with
  `csrf_header_name` set it attaches the anti-CSRF proof on POST/PUT/PATCH/DELETE
  via `ClientOptions.csrfToken`.
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
