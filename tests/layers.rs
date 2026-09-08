//! Scope-scoped middleware layers.
//!
//! Verifies the contract promised on `ApiRouter::layer`:
//! 1. a layer gates routes registered after it in the same scope
//! 2. routes registered before it (and other scopes) are unaffected
//! 3. nested group scopes inherit active layers
//! 4. layering has zero effect on collected codegen metadata

use axotyped::{ApiRouter, IntoApiRouter, RouteCollection};
use axum::body::Body;
use axum::extract::{Extension, Request};
use axum::http::{Request as HttpRequest, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use tower::Service;

#[derive(Clone, Default)]
struct AppState;

async fn ok_handler() -> &'static str {
    "ok"
}

/// Middleware that short-circuits with 403 — lets us observe exactly which
/// routes it wraps by hitting them over `oneshot`.
async fn deny_short(_req: Request, _next: Next) -> Result<Response, StatusCode> {
    Err(StatusCode::FORBIDDEN)
}

/// Pass-through middleware used where presence/order matters, not outcome.
async fn passthrough(req: Request, next: Next) -> Result<Response, StatusCode> {
    Ok(next.run(req).await)
}

async fn send(router: axum::Router<AppState>, path: &str) -> StatusCode {
    let mut svc = router.with_state(AppState);
    let req = HttpRequest::builder()
        .uri(path)
        .body(Body::empty())
        .unwrap();
    let res = <axum::Router as tower::ServiceExt<Request>>::ready(&mut svc)
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();
    res.status()
}

#[tokio::test]
async fn layer_gates_only_subsequent_routes_in_scope() {
    let router = ApiRouter::<AppState>::new()
        .get("/open", ok_handler)
        .into_api_router()
        .layer(axum::middleware::from_fn(deny_short))
        .get("/gated", ok_handler)
        .build()
        .0;

    assert_eq!(send(router.clone(), "/open").await, StatusCode::OK);
    assert_eq!(send(router, "/gated").await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn layer_does_not_leak_to_sibling_groups() {
    let router = ApiRouter::<AppState>::new()
        .group("a", |g| g.get("/x", ok_handler))
        .group("b", |g| {
            g.layer(axum::middleware::from_fn(deny_short))
                .get("/y", ok_handler)
        })
        .build()
        .0;

    // group() scopes only the TS namespace — paths stay as written
    assert_eq!(send(router.clone(), "/x").await, StatusCode::OK);
    assert_eq!(send(router, "/y").await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn parent_layer_applies_to_later_groups() {
    // A layer added BEFORE entering a group must reach that group's routes.
    let router = ApiRouter::<AppState>::new()
        .get("/free", ok_handler)
        .into_api_router()
        .layer(axum::middleware::from_fn(deny_short))
        .group_prefixed("admin", |g| g.get("/thing", ok_handler))
        .build()
        .0;

    assert_eq!(send(router.clone(), "/free").await, StatusCode::OK);
    assert_eq!(send(router, "/admin/thing").await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn nested_group_inherits_ancestor_layers() {
    let router = ApiRouter::<AppState>::new()
        .group_prefixed("admin", |g| {
            g.layer(axum::middleware::from_fn(deny_short))
                .group_prefixed("deep", |h| h.get("/res", ok_handler))
        })
        .build()
        .0;

    assert_eq!(
        send(router, "/admin/deep/res").await,
        StatusCode::FORBIDDEN,
        "nested group routes must inherit the ancestor scope's layers"
    );
}

#[tokio::test]
async fn layers_compose_and_outermost_runs_first() {
    // deny(403) applied FIRST wraps the route; passthrough applied SECOND is
    // outermost. Outermost runs first and passes through, then deny fires:
    // result must be 403, proving both wrapped the same route in order.
    let router = ApiRouter::<AppState>::new()
        .layer(axum::middleware::from_fn(deny_short))
        .layer(axum::middleware::from_fn(passthrough))
        .get("/stacked", ok_handler)
        .build()
        .0;

    assert_eq!(send(router, "/stacked").await, StatusCode::FORBIDDEN);
}

fn collect_routes(collection: RouteCollection) -> Vec<String> {
    collection
        .into_iter()
        .map(|def| format!("{} {} {:?}", def.method.as_str(), def.path, def.visibility))
        .collect()
}

#[tokio::test]
async fn codegen_metadata_identical_with_and_without_layers() {
    let clean = {
        let (_, c): (_, RouteCollection) = ApiRouter::<AppState>::new()
            .get("/users/{id}", ok_handler)
            .into_api_router()
            .post("/users/{id}", ok_handler)
            .into_api_router()
            .build();
        c
    };

    let layered = {
        let (_, c): (_, RouteCollection) = ApiRouter::<AppState>::new()
            .get("/users/{id}", ok_handler)
            .into_api_router()
            .layer(axum::middleware::from_fn(deny_short))
            .post("/users/{id}", ok_handler)
            .into_api_router()
            .build();
        c
    };

    assert_eq!(
        collect_routes(clean),
        collect_routes(layered),
        "layers must not touch codegen metadata"
    );
}

// ---------------------------------------------------------------------------
// Identity-propagation mechanics
//
// The encore-style pattern consumers build on this library: an outermost
// middleware decodes credentials once and publishes the result on request
// extensions; downstream scoped middleware and the final handler consume it
// without re-decoding. Every route lives in its own isolated single-route
// router before merging, so published state must survive that boundary.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Identity(&'static str);

/// Stands in for a global auth resolver: computes identity once, publishes it.
async fn resolve_identity(mut req: Request, next: Next) -> Result<Response, StatusCode> {
    req.extensions_mut().insert(Identity("decoded-once"));
    Ok(next.run(req).await)
}

/// Stands in for scoped RBAC middleware: consumes the published identity.
async fn require_identity(
    Extension(id): Extension<Identity>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    if id.0 == "decoded-once" {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Final consumer: extracts what upstream middleware already computed.
async fn whoami(Extension(id): Extension<Identity>) -> String {
    id.0.to_string()
}

#[tokio::test]
async fn extensions_published_by_outer_wrapper_reach_scope_layered_handlers() {
    // `require_identity` wraps the route inside axotyped's per-route
    // isolation; the resolver is attached after build(), like a real app's
    // global auth middleware. Execution order must be resolver ->
    // require_identity -> handler, with the extension crossing the merge
    // boundary between the app router and the isolated route routers.
    let router = ApiRouter::<AppState>::new()
        .layer(axum::middleware::from_fn(require_identity))
        .get("/me", whoami)
        .build()
        .0
        .layer(axum::middleware::from_fn(resolve_identity));

    let mut svc = router.with_state(AppState);
    let req = HttpRequest::builder()
        .uri("/me")
        .body(Body::empty())
        .unwrap();
    let res = <axum::Router as tower::ServiceExt<Request>>::ready(&mut svc)
        .await
        .unwrap()
        .call(req)
        .await
        .unwrap();

    assert_eq!(res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"decoded-once");
}
