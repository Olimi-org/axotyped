//! Axum router builder that collects route metadata alongside real routing.
//!
//! This module provides [`ApiRouter`], a wrapper around [`axum::Router`] that
//! builds both an Axum router and a [`RouteCollection`] from a single definition.
//! Requires the `axum` feature.
//!
//! # Example
//!
//! ```rust,ignore
//! use axotyped::ApiRouter;
//!
//! // Manual type specification (still supported):
//! // Routes are PRIVATE by default; use group_public/group_permissive or
//! // #[endpoint(public|permissive)] to open or soften them.
//! let (router, routes) = ApiRouter::<AppState>::new()
//!     .get("/users", list_users)
//!         .response::<Vec<UserResponse>>()
//!         .done()
//!     .build();
//!
//! // Auto-inferred types via #[endpoint] + register!():
//! let (router, routes) = ApiRouter::<AppState>::new()
//!     .get("/users", register!(list_users))
//!         .done()
//!     .build();
//! ```

use std::marker::PhantomData;

use axum::Router;
use axum::handler::Handler;
use axum::routing::{self, MethodRouter};

use crate::types::TypeRegistry;
use crate::types::{Collector, HttpMethod, NoCollect, RouteCollection, RouteDefinition, Visibility};

// ---------------------------------------------------------------------------
// Layer support
// ---------------------------------------------------------------------------

/// Type-erased layer application.
type LayerApplier<S> = std::sync::Arc<dyn Fn(Router<S>) -> Router<S> + Send + Sync>;

/// Apply every active scope layer to a freshly-built single-route router.
fn apply_scope_layers<S>(router: Router<S>, layers: &[LayerApplier<S>]) -> Router<S> {
    layers.iter().fold(router, |r, apply| apply(r))
}

// ---------------------------------------------------------------------------
// Type-collection trait bound
// ---------------------------------------------------------------------------

/// Trait bound for types that can be collected into the TypeRegistry.
pub trait MaybeTs: crate::ts::TS {}

impl<T: crate::ts::TS> MaybeTs for T {}

// ---------------------------------------------------------------------------
// Registered handler + IntoEndpointHandler
// ---------------------------------------------------------------------------

/// Wrapper carrying an endpoint's inferred type metadata. Produced by
/// [`register!`](crate::register).
pub struct Registered<H, Meta: crate::EndpointMeta> {
    handler: H,
    _meta: PhantomData<Meta>,
}

impl<H, Meta: crate::EndpointMeta> Registered<H, Meta> {
    /// Wrap a handler with its endpoint metadata. Called by `register!`.
    pub fn new(handler: H) -> Self {
        Self {
            handler,
            _meta: PhantomData,
        }
    }
}

/// Anything passable to a router method (`.post`, `.get`, …) as a handler.
pub trait IntoEndpointHandler<S, T> {
    /// The underlying axum handler type.
    type Handler;

    /// The handler's `#[endpoint]` metadata type.
    type Meta: crate::EndpointMeta;

    /// Extract the handler for routing.
    fn into_handler(self) -> Self::Handler;

    /// Apply this endpoint's inferred type metadata through `collector`.
    fn apply_meta<C: Collector>(def: &mut RouteDefinition, collector: &mut C);
}

// Raw handlers are not registrable: without `#[endpoint]` there is no
// metadata for the client generator.
impl<H, Meta, S, T> IntoEndpointHandler<S, T> for Registered<H, Meta>
where
    Meta: crate::EndpointMeta,
{
    type Handler = H;
    type Meta = Meta;
    fn into_handler(self) -> Self::Handler {
        self.handler
    }

    fn apply_meta<C: Collector>(def: &mut RouteDefinition, collector: &mut C) {
        <Meta as crate::EndpointMeta>::apply::<C>(def, collector);
    }
}

// ---------------------------------------------------------------------------
// String helpers
// ---------------------------------------------------------------------------

/// Strip module paths from `std::any::type_name` output.
///
/// `"alloc::vec::Vec<myapp::types::UserResponse>"` → `"Vec<UserResponse>"`
fn strip_module_paths(type_name: &str) -> String {
    let mut result = String::with_capacity(type_name.len());
    let mut last_colon_end = 0;
    let bytes = type_name.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b':' && i + 1 < bytes.len() && bytes[i + 1] == b':' {
            last_colon_end = i + 2;
            i += 2;
        } else if bytes[i] == b'<' || bytes[i] == b'>' || bytes[i] == b',' || bytes[i] == b' ' {
            if last_colon_end <= i {
                result.push_str(&type_name[last_colon_end..i]);
            }
            result.push(bytes[i] as char);
            last_colon_end = i + 1;
            i += 1;
        } else {
            i += 1;
        }
    }

    if last_colon_end <= bytes.len() {
        result.push_str(&type_name[last_colon_end..]);
    }

    result
}

/// Get the stripped type name for a Rust type.
pub fn type_string<T: 'static>() -> String {
    strip_module_paths(std::any::type_name::<T>())
}

/// Extract the function name from `std::any::type_name` on a function item.
///
/// `"myapp::plugins::email_password::register"` → `"register"`
fn handler_name_from_type_name(type_name: &str) -> &str {
    // Function type names can have suffixes like `::{{closure}}`, strip those.
    type_name
        .rsplit("::")
        .find(|s| !s.starts_with('{'))
        .unwrap_or(type_name)
}

/// Convert `snake_case` to `camelCase`.
///
/// `"forgot_password"` → `"forgotPassword"`
/// `"list_users"` → `"listUsers"`
/// `"register"` → `"register"` (no-op)
pub(crate) fn snake_to_camel(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut capitalize_next = false;

    for ch in s.chars() {
        if ch == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(ch.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(ch);
        }
    }

    result
}

/// Derive a camelCase client method name from a handler function's type name.
pub(crate) fn default_name_from_handler<H: 'static>() -> String {
    let full = std::any::type_name::<H>();
    let raw = handler_name_from_type_name(full);
    snake_to_camel(raw)
}

// ---------------------------------------------------------------------------
// ApiRouter
// ---------------------------------------------------------------------------

/// Builder producing both an [`axum::Router`] and a [`RouteCollection`].
///
/// Generic over state `S` and [`Collector`] `C` (`NoCollect` by default;
/// `TypeRegistry` to collect types for binding generation).
#[derive(Debug, Clone, Copy, Default)]
struct Scope {
    /// Declared visibility for routes registered in this scope.
    visibility: Visibility,
    /// Whether an `auth_layer` is active in this scope.
    protected: bool,
}

impl Scope {
    /// Resolve to `(declared, effective)` visibility: protection forces
    /// effective `Private` while the declaration is preserved for
    /// diagnostics.
    fn resolve(self) -> (Visibility, Visibility) {
        let declared = self.visibility;
        let effective = if self.protected {
            Visibility::Private
        } else {
            declared
        };
        (declared, effective)
    }
}
pub struct ApiRouter<S = (), C: Collector = NoCollect, AuthScope = crate::params::PrivateScope> {
    router: Router<S>,
    routes: Vec<RouteDefinition>,
    collector: C,
    current_group: Option<String>,
    current_prefix: Option<String>,
    /// Visibility state for routes registered in this scope.
    scope: Scope,
    /// Layers applied to routes registered after they are added.
    layers: Vec<LayerApplier<S>>,
    /// Declared credential lifecycle. Root-level: one declaration per router
    /// tree, validated against the full route table at `build()`.
    auth_flow: Option<crate::auth_flow::AuthFlow>,
    /// Type-level registration scope — decides which handlers compile here
    /// (see [`AllowsRoute`](crate::AllowsRoute)).
    _auth_scope: std::marker::PhantomData<AuthScope>,
}

/// Constructors accept initial scopes only.
impl<S, C, AuthScope: crate::params::InitialScope> ApiRouter<S, C, AuthScope>
where
    S: Clone + Send + Sync + 'static,
    C: Collector,
{
    /// Create a new empty builder.
    pub fn new() -> Self {
        Self {
            router: Router::new(),
            routes: Vec::new(),
            collector: C::default(),
            current_group: None,
            current_prefix: None,
            scope: Scope::default(),
            layers: Vec::new(),
            auth_flow: None,
            _auth_scope: PhantomData,
        }
    }

    /// Re-materialize a router from scope-erased parts.
    pub fn from_parts(parts: RouterParts<S, C>) -> ApiRouter<S, C, AuthScope> {
        ApiRouter {
            router: parts.router,
            routes: parts.routes,
            collector: parts.collector,
            current_group: None,
            current_prefix: None,
            scope: Scope::default(),
            layers: Vec::new(),
            auth_flow: parts.auth_flow,
            _auth_scope: PhantomData,
        }
    }
}

impl<S, C, AuthScope> ApiRouter<S, C, AuthScope>
where
    S: Clone + Send + Sync + 'static,
    C: Collector,
{
    /// Swap the type-level scope (used by `auth_layer`).
    fn retarget_scope<NewScope>(self) -> ApiRouter<S, C, NewScope> {
        ApiRouter {
            router: self.router,
            routes: self.routes,
            collector: self.collector,
            current_group: self.current_group,
            current_prefix: self.current_prefix,
            scope: self.scope,
            layers: self.layers,
            auth_flow: self.auth_flow,
            _auth_scope: PhantomData,
        }
    }

    /// Declare this API's credential lifecycle. Validated at `build()`.
    /// One declaration per router tree.
    ///
    /// # Example
    /// ```rust,ignore
    /// r.auth_flow(SessionAuth::flow())
    ///     .get("/auth/me", register!(me)) // permissive bootstrap
    /// ```
    pub fn auth_flow(mut self, flow: crate::auth_flow::AuthFlow) -> Self {
        if let Some(existing) = &self.auth_flow {
            panic!(
                "two auth flows declared ('{}' and '{}'); \
                 an API has one credential lifecycle",
                existing.name, flow.name
            );
        }
        self.auth_flow = Some(flow);
        self
    }

    /// Borrow the collector (e.g. to call `export` after building).
    pub fn collector(&self) -> &C {
        &self.collector
    }

    /// Set a URL prefix for all subsequent routes.
    ///
    /// Paths registered via `.post()`, `.get()`, etc. will have this prefix
    /// prepended automatically. For example, `.set_prefix("/admin")` followed
    /// by `.post("/course", ...)` registers `/admin/course`.
    pub fn set_prefix(mut self, prefix: &str) -> Self {
        self.current_prefix = Some(prefix.to_string());
        self
    }

    /// Closure-based group of public routes. No URL prefix is added.
    pub fn group_public<R, F>(self, name: &str, routes: F) -> Self
    where
        F: FnOnce(ApiRouter<S, C, crate::params::PublicScope>) -> R,
        R: IntoApiRouterAny<S, C>,
    {
        let inner = ApiRouter {
            router: Router::new(),
            routes: Vec::new(),
            collector: C::default(),
            current_group: Some(name.to_string()),
            current_prefix: self.current_prefix.clone(),
            scope: Scope {
                visibility: Visibility::Public,
                ..self.scope
            },
            layers: self.layers.clone(),
            auth_flow: None,
            _auth_scope: PhantomData,
        };

        let parts = routes(inner).into_router_parts();

        self.absorb_parts(parts)
    }

    /// Closure-based group of permissive routes (credentials attached when
    /// available, never required).
    pub fn group_permissive<R, F>(self, name: &str, routes: F) -> Self
    where
        F: FnOnce(ApiRouter<S, C, crate::params::PermissiveScope>) -> R,
        R: IntoApiRouterAny<S, C>,
    {
        let inner = ApiRouter {
            router: Router::new(),
            routes: Vec::new(),
            collector: C::default(),
            current_group: Some(name.to_string()),
            current_prefix: self.current_prefix.clone(),
            scope: Scope {
                visibility: Visibility::Permissive,
                ..self.scope
            },
            layers: self.layers.clone(),
            auth_flow: None,
            _auth_scope: PhantomData,
        };

        let parts = routes(inner).into_router_parts();

        self.absorb_parts(parts)
    }

    /// Register a tower layer for routes added after this call within the
    /// current scope. Applies per-route; does not leak to parents or
    /// siblings; has no effect on codegen.
    pub fn layer<L>(mut self, layer: L) -> Self
    where
        L: tower::Layer<axum::routing::Route> + Clone + Send + Sync + 'static,
        L::Service: tower::Service<axum::extract::Request, Error = std::convert::Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
        <L::Service as tower::Service<axum::extract::Request>>::Response:
            axum::response::IntoResponse,
        <L::Service as tower::Service<axum::extract::Request>>::Future: Send + 'static,
    {
        self.layers
            .push(std::sync::Arc::new(move |router: Router<S>| {
                router.layer(layer.clone())
            }));
        self
    }

    /// Like [`layer`](Self::layer), but marks scoped routes `Private`.
    /// Contradictions against public declarations are reported by
    /// [`generate_with_warnings`](crate::generate_with_warnings).
    pub fn auth_layer<L>(mut self, layer: L) -> ApiRouter<S, C, crate::params::LayeredAuthScope>
    where
        L: tower::Layer<axum::routing::Route> + Clone + Send + Sync + 'static,
        L::Service: tower::Service<axum::extract::Request, Error = std::convert::Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
        <L::Service as tower::Service<axum::extract::Request>>::Response:
            axum::response::IntoResponse,
        <L::Service as tower::Service<axum::extract::Request>>::Future: Send + 'static,
    {
        self.scope.protected = true;
        self.retarget_scope().layer(layer)
    }

    /// Group with scoped TS namespace; does not modify path prefixes.
    /// See [`group_prefixed`](Self::group_prefixed) for the prefixed variant.
    pub fn group<R, F>(self, name: &str, routes: F) -> Self
    where
        F: FnOnce(ApiRouter<S, C, AuthScope>) -> R,
        R: IntoApiRouterAny<S, C>,
    {
        let inner = ApiRouter {
            router: Router::new(),
            routes: Vec::new(),
            collector: C::default(),
            current_group: Some(name.to_string()),
            current_prefix: self.current_prefix.clone(),
            scope: self.scope,
            layers: self.layers.clone(),
            auth_flow: None,
            _auth_scope: PhantomData,
        };

        let parts = routes(inner).into_router_parts();

        self.absorb_parts(parts)
    }

    /// Group with scoped TS namespace and URL prefix (`/{name}`).
    /// Config does not leak past the closure.
    pub fn group_prefixed<R, F>(self, name: &str, routes: F) -> Self
    where
        F: FnOnce(ApiRouter<S, C, AuthScope>) -> R,
        R: IntoApiRouterAny<S, C>,
    {
        let default_prefix = match &self.current_prefix {
            Some(p) => format!("{}/{}", p.trim_end_matches('/'), name),
            None => format!("/{}", name),
        };
        let inner = ApiRouter {
            router: Router::new(),
            routes: Vec::new(),
            collector: C::default(),
            current_group: Some(name.to_string()),
            current_prefix: Some(default_prefix),
            scope: self.scope,
            layers: self.layers.clone(),
            auth_flow: None,
            _auth_scope: PhantomData,
        };

        let parts = routes(inner).into_router_parts();

        self.absorb_parts(parts)
    }

    /// Merge subtree parts: routes/types fold in; a subtree flow is adopted
    /// or panics against an existing one.
    fn absorb_parts(mut self, parts: RouterParts<S, C>) -> Self {
        if let Some(flow) = parts.auth_flow {
            if let Some(existing) = &self.auth_flow {
                panic!(
                    "two auth flows declared ('{}' and '{}'); \
                     an API has one credential lifecycle",
                    existing.name, flow.name
                );
            }
            self.auth_flow = Some(flow);
        }
        self.router = self.router.merge(parts.router);
        self.routes.extend(parts.routes);
        self.collector.merge_collection(parts.collector);
        self
    }

    /// Merge another `ApiRouter`'s router and routes into this one.
    pub fn merge(self, other: ApiRouter<S, C, AuthScope>) -> Self {
        self.absorb_parts(other.into_router_parts())
    }

    /// Consume the builder, returning router and route metadata.
    /// Validates any [`auth_flow`](Self::auth_flow) declaration first.
    pub fn build(self) -> (Router<S>, RouteCollection) {
        if let Some(flow) = &self.auth_flow {
            if let Err(e) = flow.validate(&self.routes) {
                panic!("auth_scheme '{}': {e}", flow.name);
            }
        }
        // Client method names are unique per group.
        {
            let mut seen = std::collections::HashMap::<(&str, &str), &str>::new();
            for r in &self.routes {
                let key = (r.group.as_deref().unwrap_or(""), r.name.as_str());
                if let Some(prev_path) = seen.insert(key, r.path.as_str()) {
                    panic!(
                        "two routes share the client method name '{}'{}: {} vs {}",
                        r.name,
                        if key.0.is_empty() { String::new() }
                        else { format!(" in group '{}'", key.0) },
                        prev_path,
                        r.path,
                    );
                }
            }
        }
        let collection = RouteCollection::assemble(
            self.routes,
            self.collector.into_type_registry(),
            self.auth_flow,
        );
        (self.router, collection)
    }

    // --- Standard HTTP method helpers ---

    /// Shared registration core for the HTTP method helpers.
    fn register<EH, T>(
        mut self,
        path: &str,
        method: HttpMethod,
        to_method_router: fn(EH::Handler) -> MethodRouter<S>,
        ep: EH,
    ) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
    {
        let mut def = route_into_def(
            &mut self.router,
            path,
            method,
            to_method_router,
            ep,
            &self.current_prefix,
            self.scope,
            &self.current_group,
            false,
            &self.layers,
        );
        EH::apply_meta::<C>(&mut def, &mut self.collector);
        if self.scope.protected {
            def.visibility = Visibility::Private;
        }
        RouteBuilder { parent: self, def }
    }

    /// Add a GET route.
    pub fn get<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
    {
        self.register(path, HttpMethod::Get, routing::get, ep)
    }

    /// Add a POST route.
    pub fn post<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
    {
        self.register(path, HttpMethod::Post, routing::post, ep)
    }

    /// Add a PUT route.
    pub fn put<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
    {
        self.register(path, HttpMethod::Put, routing::put, ep)
    }

    /// Add a PATCH route.
    pub fn patch<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
    {
        self.register(path, HttpMethod::Patch, routing::patch, ep)
    }

    /// Add a DELETE route.
    pub fn delete<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
    {
        self.register(path, HttpMethod::Delete, routing::delete, ep)
    }

    /// Add a WebSocket route.
    pub fn ws<EH, T>(mut self, path: &str, ep: EH) -> WsRouteBuilder<S, C, AuthScope, crate::params::NoEvents>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
    {
        let mut def = route_into_def(
            &mut self.router,
            path,
            HttpMethod::Get,
            routing::get,
            ep,
            &self.current_prefix,
            self.scope,
            &self.current_group,
            true,
            &self.layers,
        );
        EH::apply_meta::<C>(&mut def, &mut self.collector);
        if self.scope.protected {
            def.visibility = Visibility::Private;
        }
        WsRouteBuilder {
            parent: self,
            def,
            _events: std::marker::PhantomData,
        }
    }
}

/// Resolves `path` against an optional prefix.
fn resolve_prefix(prefix: &Option<String>, path: &str) -> String {
    match prefix {
        Some(prefix) => format!("{}{}", prefix, path),
        None => path.to_string(),
    }
}

/// Register `ep` into `router` and build the initial [`RouteDefinition`].
fn route_into_def<S, EH, T>(
    router: &mut Router<S>,
    path: &str,
    method: HttpMethod,
    to_method_router: fn(EH::Handler) -> MethodRouter<S>,
    ep: EH,
    prefix: &Option<String>,
    scope: Scope,
    group: &Option<String>,
    websocket: bool,
    layers: &[LayerApplier<S>],
) -> RouteDefinition
where
    S: Clone + Send + Sync + 'static,
    EH: IntoEndpointHandler<S, T>,
    EH::Handler: Handler<T, S> + 'static,
    T: 'static,
{
    let name = default_name_from_handler::<EH::Handler>();
    let handler = ep.into_handler();
    let full_path = resolve_prefix(prefix, path);
    let path_params = crate::extract_path_params(&full_path);
    let mini = Router::<S>::new().route(&full_path, to_method_router(handler));
    let layered = apply_scope_layers(mini, layers);
    *router = std::mem::take(router).merge(layered);
    let (declared, visibility) = scope.resolve();
    RouteDefinition {
        name,
        method,
        path: full_path,
        visibility,
        declared,
        body_type: None,
        response_type: None,
        query_type: None,
        path_params,
        group: group.clone(),
        allow_redirects: false,
        redirect: false,
        websocket,
        ws_send_type: None,
        ws_receive_type: None,
    }
}

/// Scope-erased router contents.
pub struct RouterParts<S, C: Collector> {
    pub router: Router<S>,
    pub routes: Vec<RouteDefinition>,
    pub collector: C,
    pub auth_flow: Option<crate::auth_flow::AuthFlow>,
}

impl<S, C: Collector, Sc> From<ApiRouter<S, C, Sc>> for RouterParts<S, C> {
    fn from(r: ApiRouter<S, C, Sc>) -> Self {
        Self {
            router: r.router,
            routes: r.routes,
            collector: r.collector,
            auth_flow: r.auth_flow,
        }
    }
}

/// Anything a group closure can end in.
pub trait IntoApiRouterAny<S, C: Collector> {
    fn into_router_parts(self) -> RouterParts<S, C>;
}

impl<S, C: Collector, Sc> IntoApiRouterAny<S, C> for ApiRouter<S, C, Sc> {
    fn into_router_parts(self) -> RouterParts<S, C> {
        self.into()
    }
}

impl<S, C, Sc> IntoApiRouterAny<S, C> for RouteBuilder<S, C, Sc>
where
    S: Clone + Send + Sync + 'static,
    C: Collector,
{
    fn into_router_parts(self) -> RouterParts<S, C> {
        self.done().into()
    }
}

impl<S, C, Sc> IntoApiRouterAny<S, C> for WsRouteBuilder<S, C, Sc, crate::params::EventsDeclared>
where
    S: Clone + Send + Sync + 'static,
    C: Collector,
{
    fn into_router_parts(self) -> RouterParts<S, C> {
        self.done().into()
    }
}

/// Build the lean server [`Router`] from a route-definition function.
/// No type collection happens.
pub fn build_routes<S, R, F>(define: F) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    F: FnOnce(ApiRouter<S, NoCollect>) -> R,
    R: IntoApiRouterAny<S, NoCollect>,
{
    ApiRouter::<S, NoCollect>::from_parts(define(ApiRouter::<S, NoCollect>::new()).into_router_parts())
        .build()
        .0
}

/// Collect route types from the same route-definition function passed to
/// [`build_routes`].
pub fn collect_routes<S, R, F>(define: F) -> RouteCollection
where
    S: Clone + Send + Sync + 'static,
    F: FnOnce(ApiRouter<S, TypeRegistry>) -> R,
    R: IntoApiRouterAny<S, TypeRegistry>,
{
    ApiRouter::<S, TypeRegistry>::from_parts(define(ApiRouter::<S, TypeRegistry>::new()).into_router_parts())
        .build()
        .1
}

/// Build both the server [`Router`] and the collected [`RouteCollection`]
/// from a single collecting pass.
pub fn build_typed<S, R, F>(define: F) -> (Router<S>, RouteCollection)
where
    S: Clone + Send + Sync + 'static,
    F: FnOnce(ApiRouter<S, TypeRegistry>) -> R,
    R: IntoApiRouterAny<S, TypeRegistry>,
{
    ApiRouter::<S, TypeRegistry>::from_parts(define(ApiRouter::<S, TypeRegistry>::new()).into_router_parts())
        .build()
}

/// Trait for declaring application route tables.
pub trait RouteTable<S> {
    /// Declare routes on the given [`ApiRouter`].
    fn define<C: Collector>(r: ApiRouter<S, C>) -> ApiRouter<S, C>;

    /// Build the lean production server [`Router`].
    fn router() -> Router<S>
    where
        S: Clone + Send + Sync + 'static,
    {
        build_routes(|r| Self::define(r))
    }

    /// Collect TypeScript type data for binding generation.
    fn collect_types() -> RouteCollection
    where
        S: Clone + Send + Sync + 'static,
    {
        collect_routes(|r| Self::define(r))
    }

    /// Collecting build returning [`Router`] and [`RouteCollection`].
    fn build() -> (Router<S>, RouteCollection)
    where
        S: Clone + Send + Sync + 'static,
    {
        build_typed(|r| Self::define(r))
    }

}

impl<S, C, AuthScope: crate::params::InitialScope> Default for ApiRouter<S, C, AuthScope>
where
    S: Clone + Send + Sync + 'static,
    C: Collector,
{
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// RouteBuilder
// ---------------------------------------------------------------------------

/// In-progress route definition. Chain `.body::<T>()`, `.response::<T>()`,
/// `.redirect()`, then finalize with `.done()` or `.as_("name")`.
pub struct RouteBuilder<S, C: Collector = NoCollect, AuthScope = crate::params::PrivateScope> {
    parent: ApiRouter<S, C, AuthScope>,
    def: RouteDefinition,
}

impl<S, C, AuthScope> RouteBuilder<S, C, AuthScope>
where
    S: Clone + Send + Sync + 'static,
    C: Collector,
{
    /// Set the request body type.
    pub fn body<T: MaybeTs + 'static>(mut self) -> Self {
        self.def.body_type = Some(type_string::<T>());
        self.parent.collector.register::<T>();
        self
    }

    /// Set the response type.
    pub fn response<T: MaybeTs + 'static>(mut self) -> Self {
        self.def.response_type = Some(type_string::<T>());
        self.parent.collector.register::<T>();
        self
    }

    /// Set the query parameters type.
    pub fn query<T: MaybeTs + 'static>(mut self) -> Self {
        self.def.query_type = Some(type_string::<T>());
        self.parent.collector.register::<T>();
        self
    }

    /// Set both body and response types at once.
    pub fn json<B: MaybeTs + 'static, R: MaybeTs + 'static>(mut self) -> Self {
        self.def.body_type = Some(type_string::<B>());
        self.def.response_type = Some(type_string::<R>());
        self.parent.collector.register::<B>();
        self.parent.collector.register::<R>();
        self
    }

    /// Mark this route as a browser redirect (URL builder, not fetch).
    pub fn redirect(mut self) -> Self {
        self.def.redirect = true;
        self
    }

    /// Drop the route's body/query/response metadata from the generated
    /// client.
    pub fn opaque(mut self) -> Self {
        self.def.body_type = None;
        self.def.query_type = None;
        self.def.response_type = None;
        self
    }

    /// Allow this route to follow redirects. Applies to credentialless
    /// calls only.
    pub fn allow_redirects(mut self) -> Self {
        self.def.allow_redirects = true;
        self
    }

    fn done(mut self) -> ApiRouter<S, C, AuthScope> {
        // name was already set from the handler in ApiRouter::register()
        self.parent.routes.push(self.def);
        self.parent
    }

    /// Finalize the route with an explicit client method name.
    pub fn as_(mut self, name: &str) -> ApiRouter<S, C, AuthScope> {
        self.def.name = name.to_string();
        self.parent.routes.push(self.def);
        self.parent
    }

    // --- Auto-closing forwarding route methods ---

    /// Add a GET route.
    pub fn get<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
    {
        self.done().get(path, ep)
    }

    /// Add a POST route.
    pub fn post<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
    {
        self.done().post(path, ep)
    }

    /// Add a PUT route.
    pub fn put<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
    {
        self.done().put(path, ep)
    }

    /// Add a PATCH route.
    pub fn patch<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
    {
        self.done().patch(path, ep)
    }

    /// Add a DELETE route.
    pub fn delete<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
    {
        self.done().delete(path, ep)
    }

    /// Add a WebSocket route.
    pub fn ws<EH, T>(self, path: &str, ep: EH) -> WsRouteBuilder<S, C, AuthScope, crate::params::NoEvents>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
    {
        self.done().ws(path, ep)
    }

    /// Like [`layer`](ApiRouter::layer).
    pub fn layer<L>(self, layer: L) -> ApiRouter<S, C, AuthScope>
    where
        L: tower::Layer<axum::routing::Route> + Clone + Send + Sync + 'static,
        L::Service: tower::Service<axum::extract::Request, Error = std::convert::Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
        <L::Service as tower::Service<axum::extract::Request>>::Response:
            axum::response::IntoResponse,
        <L::Service as tower::Service<axum::extract::Request>>::Future: Send + 'static,
    {
        self.done().layer(layer)
    }

    /// Like [`auth_layer`](ApiRouter::auth_layer).
    pub fn auth_layer<L>(
        self,
        layer: L,
    ) -> ApiRouter<S, C, crate::params::LayeredAuthScope>
    where
        L: tower::Layer<axum::routing::Route> + Clone + Send + Sync + 'static,
        L::Service: tower::Service<axum::extract::Request, Error = std::convert::Infallible>
            + Clone
            + Send
            + Sync
            + 'static,
        <L::Service as tower::Service<axum::extract::Request>>::Response:
            axum::response::IntoResponse,
        <L::Service as tower::Service<axum::extract::Request>>::Future: Send + 'static,
    {
        self.done().auth_layer(layer)
    }

    /// Scoped closure-based group.
    pub fn group<R, F>(self, name: &str, routes: F) -> ApiRouter<S, C, AuthScope>
    where
        F: FnOnce(ApiRouter<S, C, AuthScope>) -> R,
        R: IntoApiRouterAny<S, C>,
    {
        self.done().group(name, routes)
    }

    /// Scoped closure-based prefixed group.
    pub fn group_prefixed<R, F>(self, name: &str, routes: F) -> ApiRouter<S, C, AuthScope>
    where
        F: FnOnce(ApiRouter<S, C, AuthScope>) -> R,
        R: IntoApiRouterAny<S, C>,
    {
        self.done().group_prefixed(name, routes)
    }

    /// Scoped closure-based permissive group.
    pub fn group_permissive<R, F>(self, name: &str, routes: F) -> ApiRouter<S, C, AuthScope>
    where
        F: FnOnce(ApiRouter<S, C, crate::params::PermissiveScope>) -> R,
        R: IntoApiRouterAny<S, C>,
    {
        self.done().group_permissive(name, routes)
    }

    /// Scoped closure-based public group.
    pub fn group_public<R, F>(self, name: &str, routes: F) -> ApiRouter<S, C, AuthScope>
    where
        F: FnOnce(ApiRouter<S, C, crate::params::PublicScope>) -> R,
        R: IntoApiRouterAny<S, C>,
    {
        self.done().group_public(name, routes)
    }

    /// Set a URL prefix on the parent router.
    pub fn set_prefix(self, prefix: &str) -> ApiRouter<S, C, AuthScope> {
        self.done().set_prefix(prefix)
    }

    /// Finalize the route and consume the builder.
    pub fn build(self) -> (Router<S>, RouteCollection) {
        self.done().build()
    }
}

// ---------------------------------------------------------------------------
// WsRouteBuilder
// ---------------------------------------------------------------------------

/// Builder for a WebSocket route. Call `.events::<S, R>()` before leaving
/// the chain.
///
/// ```compile_fail
/// use axotyped::{ApiRouter, register};
///
/// // `public`, so registration is allowed and the ONLY compile error is
/// // the one this doctest pins: leaving the chain before declaring events.
/// #[axotyped::endpoint(public)]
/// async fn socket() {}
///
/// let _ = ApiRouter::<()>::new()
///     .ws("/raw", register!(socket))
///     .get("/x", register!(socket)) // ERROR: events not declared
///     .build();
/// ```
pub struct WsRouteBuilder<
    S,
    C: Collector = NoCollect,
    AuthScope = crate::params::PrivateScope,
    Events = crate::params::NoEvents,
> {
    parent: ApiRouter<S, C, AuthScope>,
    def: RouteDefinition,
    _events: std::marker::PhantomData<Events>,
}

impl<S, C, AuthScope, Events> WsRouteBuilder<S, C, AuthScope, Events>
where
    S: Clone + Send + Sync + 'static,
    C: Collector,
{
    /// Set the query parameters type.
    pub fn query<T: MaybeTs + 'static>(mut self) -> Self {
        self.def.query_type = Some(type_string::<T>());
        self.parent.collector.register::<T>();
        self
    }

    /// Set the event types for this WebSocket endpoint.
    pub fn events<Send: MaybeTs + 'static, Receive: MaybeTs + 'static>(
        self,
    ) -> WsRouteBuilder<S, C, AuthScope, crate::params::EventsDeclared> {
        let mut this = self;
        this.def.ws_send_type = Some(type_string::<Send>());
        this.def.ws_receive_type = Some(type_string::<Receive>());
        this.parent.collector.register::<Send>();
        this.parent.collector.register::<Receive>();
        let WsRouteBuilder { parent, def, .. } = this;
        WsRouteBuilder {
            parent,
            def,
            _events: std::marker::PhantomData,
        }
    }

    fn done(mut self) -> ApiRouter<S, C, AuthScope>
    where
        Events: crate::params::WsEventsComplete,
    {
        self.parent.routes.push(self.def);
        self.parent
    }

    /// Finalize the route with an explicit client method name.
    pub fn as_(mut self, name: &str) -> ApiRouter<S, C, AuthScope>
    where
        Events: crate::params::WsEventsComplete,
    {
        self.def.name = name.to_string();
        self.parent.routes.push(self.def);
        self.parent
    }

    // --- Auto-closing forwarding route methods ---

    /// Add a GET route.
    pub fn get<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
        Events: crate::params::WsEventsComplete,
    {
        self.done().get(path, ep)
    }

    /// Add a POST route.
    pub fn post<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
        Events: crate::params::WsEventsComplete,
    {
        self.done().post(path, ep)
    }

    /// Add a PUT route.
    pub fn put<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
        Events: crate::params::WsEventsComplete,
    {
        self.done().put(path, ep)
    }

    /// Add a PATCH route.
    pub fn patch<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
        Events: crate::params::WsEventsComplete,
    {
        self.done().patch(path, ep)
    }

    /// Add a DELETE route.
    pub fn delete<EH, T>(self, path: &str, ep: EH) -> RouteBuilder<S, C, AuthScope>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
        Events: crate::params::WsEventsComplete,
    {
        self.done().delete(path, ep)
    }

    /// Add a WebSocket route.
    pub fn ws<EH, T>(self, path: &str, ep: EH) -> WsRouteBuilder<S, C, AuthScope, crate::params::NoEvents>
    where
        EH: IntoEndpointHandler<S, T>,
        EH::Handler: Handler<T, S> + 'static,
        T: 'static,
        AuthScope: crate::params::AllowsRoute<S, EH, T>,
        Events: crate::params::WsEventsComplete,
    {
        self.done().ws(path, ep)
    }

    /// Scoped closure-based group.
    pub fn group<R, F>(self, name: &str, routes: F) -> ApiRouter<S, C, AuthScope>
    where
        F: FnOnce(ApiRouter<S, C, AuthScope>) -> R,
        R: IntoApiRouterAny<S, C>,
        Events: crate::params::WsEventsComplete,
    {
        self.done().group(name, routes)
    }

    /// Scoped closure-based prefixed group.
    pub fn group_prefixed<R, F>(self, name: &str, routes: F) -> ApiRouter<S, C, AuthScope>
    where
        F: FnOnce(ApiRouter<S, C, AuthScope>) -> R,
        R: IntoApiRouterAny<S, C>,
        Events: crate::params::WsEventsComplete,
    {
        self.done().group_prefixed(name, routes)
    }

    /// Scoped closure-based permissive group.
    pub fn group_permissive<R, F>(self, name: &str, routes: F) -> ApiRouter<S, C, AuthScope>
    where
        F: FnOnce(ApiRouter<S, C, crate::params::PermissiveScope>) -> R,
        R: IntoApiRouterAny<S, C>,
        Events: crate::params::WsEventsComplete,
    {
        self.done().group_permissive(name, routes)
    }

    /// Scoped closure-based public group.
    pub fn group_public<R, F>(self, name: &str, routes: F) -> ApiRouter<S, C, AuthScope>
    where
        F: FnOnce(ApiRouter<S, C, crate::params::PublicScope>) -> R,
        R: IntoApiRouterAny<S, C>,
        Events: crate::params::WsEventsComplete,
    {
        self.done().group_public(name, routes)
    }

    /// Set a URL prefix on the parent router.
    pub fn set_prefix(self, prefix: &str) -> ApiRouter<S, C, AuthScope>
    where
        Events: crate::params::WsEventsComplete,
    {
        self.done().set_prefix(prefix)
    }

    /// Finalize the route and consume the builder.
    pub fn build(self) -> (Router<S>, RouteCollection)
    where
        Events: crate::params::WsEventsComplete,
    {
        self.done().build()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- strip_module_paths --

    #[test]
    fn strip_simple_type() {
        assert_eq!(
            strip_module_paths("myapp::types::UserResponse"),
            "UserResponse"
        );
    }

    #[test]
    fn strip_vec_generic() {
        assert_eq!(
            strip_module_paths("alloc::vec::Vec<myapp::types::UserResponse>"),
            "Vec<UserResponse>"
        );
    }

    #[test]
    fn strip_option_generic() {
        assert_eq!(
            strip_module_paths("core::option::Option<myapp::types::UserResponse>"),
            "Option<UserResponse>"
        );
    }

    #[test]
    fn strip_plain_type() {
        assert_eq!(strip_module_paths("String"), "String");
    }

    #[test]
    fn strip_nested_generic() {
        assert_eq!(
            strip_module_paths("alloc::vec::Vec<core::option::Option<myapp::Foo>>"),
            "Vec<Option<Foo>>"
        );
    }

    // -- type_string --

    #[test]
    fn type_string_for_vec() {
        assert_eq!(type_string::<Vec<String>>(), "Vec<String>");
    }

    #[test]
    fn type_string_for_option() {
        assert_eq!(type_string::<Option<String>>(), "Option<String>");
    }

    #[test]
    fn type_string_for_plain() {
        assert_eq!(type_string::<String>(), "String");
    }

    // -- snake_to_camel --

    #[test]
    fn camel_simple() {
        assert_eq!(snake_to_camel("register"), "register");
    }

    #[test]
    fn camel_two_words() {
        assert_eq!(snake_to_camel("forgot_password"), "forgotPassword");
    }

    #[test]
    fn camel_three_words() {
        assert_eq!(snake_to_camel("list_all_users"), "listAllUsers");
    }

    #[test]
    fn camel_already_camel() {
        assert_eq!(snake_to_camel("listUsers"), "listUsers");
    }

    // -- handler_name_from_type_name --

    #[test]
    fn handler_name_simple() {
        assert_eq!(
            handler_name_from_type_name("myapp::plugins::email_password::register"),
            "register"
        );
    }

    #[test]
    fn handler_name_nested() {
        assert_eq!(
            handler_name_from_type_name("myapp::handlers::admin::list_users"),
            "list_users"
        );
    }

    #[test]
    fn handler_name_closure() {
        assert_eq!(
            handler_name_from_type_name("myapp::routes::handler::{{closure}}"),
            "handler"
        );
    }

    // -- default_name_from_handler --

    #[test]
    fn default_name_list_users() {
        let name = default_name_from_handler::<fn()>();
        // For a plain fn() type, type_name is just the type signature, not useful.
        // The real test is with named function items in builder_tests.rs.
        // Here we just test the helpers individually.
        assert!(!name.is_empty());
    }

    #[test]
    fn default_name_via_snake_to_camel() {
        // Simulate what happens: handler type_name ends with "list_users"
        let raw = handler_name_from_type_name("myapp::handlers::list_users");
        let name = snake_to_camel(raw);
        assert_eq!(name, "listUsers");
    }

    #[test]
    fn default_name_no_underscore() {
        let raw = handler_name_from_type_name("myapp::handlers::register");
        let name = snake_to_camel(raw);
        assert_eq!(name, "register");
    }
}
