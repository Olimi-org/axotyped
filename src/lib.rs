//! # axotyped
//!
//! Auto-generate typed TypeScript API clients from Axum route metadata.
//!
//! This crate provides:
//! - An `ApiRouter` builder that creates both an Axum router and route metadata
//! - `#[endpoint]` and `register!()` macros for automatic type inference
//! - A TypeScript client code generator that produces typed fetch wrappers
//! - A `check()` function for CI that fails if the generated client is stale
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use axotyped::{ApiRouter, endpoint, register};
//!
//! #[endpoint]
//! pub async fn list_projects(
//!     State(state): State<AppState>,
//! ) -> Result<Json<Vec<ProjectResponse>>, StatusCode> {
//!     // ...
//! }
//!
//! let (router, routes) = ApiRouter::<AppState>::new()
//!     .group_prefixed("admin", |g| {
//!         g.get("/projects", register!(list_projects))
//!              .done()
//!     })
//!     .build();
//! ```

mod builder;
mod generator;
mod types;
#[macro_use]
mod macros;

mod auth_flow;
mod params;

// Re-export public API
pub use auth_flow::{AuthFlow, AuthFlowKind, endpoint_name};
pub use params::{
    AllowsRoute, OptionalCredential, Allowed, AssertCredentiallessDeclaration, AuthFlag, AuthOr,
    EndpointParam, EndpointVisibilityCoherence, Credential, FlagConst, IsCoherentWithParams,
    LayeredAuthScope, NotAllowed, PermissiveScope, PrivateScope, PublicScope,
};
pub use generator::{
    AuthScheme, CheckError, GeneratorConfig, check, generate, generate_to_file,
    generate_with_warnings,
};
pub use types::{
    Collector, HttpMethod, NoCollect, PathParam, RouteCollection, RouteDefinition, TypeRegistry,
    Visibility, extract_path_params, is_valid_js_identifier,
};

pub use builder::{
    ApiRouter, IntoApiRouterAny, IntoEndpointHandler, MaybeTs, Registered, RouteBuilder,
    RouteTable, WsRouteBuilder, build_routes,
};

pub use builder::{build_typed, collect_routes};

/// [`ApiRouter`] with [`TypeRegistry`] as the collector — the variant that collects route types
/// for ts-rs export.
///
/// Convenience alias for codegen entry points: `TypedApiRouter::<AppState>::new()` is equivalent
/// to `ApiRouter::<AppState, TypeRegistry>::new()`, and the collector type `C` can be left to
/// inference when passing the builder to a generic function.
pub type TypedApiRouter<S = ()> = ApiRouter<S, TypeRegistry>;

// Re-export the #[endpoint] attribute macro and register!() call-site macro.
pub use axotyped_macros::{credential, endpoint, optional_credential, register};

/// Declare a route table as a zero-sized type implementing [`RouteTable`].
///
/// Expands to a unit struct implementing [`RouteTable`], providing `.router()`, `.collect_types()`,
/// and `.build()` methods.
///
/// # Example
/// ```rust,ignore
/// axotyped::define_routes! {
///     pub Routes for Arc<AppState>, |r| {
///         r.get("/health", axotyped::register!(health))
///             .group_prefixed("admin", |g| g.post("/x", axotyped::register!(create_x))) // private by default
///     }
/// }
///
/// let router = Routes::router().with_state(state);
/// let collection = Routes::collect_types(); // from a debug-only codegen entry point
/// ```
#[macro_export]
macro_rules! define_routes {
    ($vis:vis $name:ident for $state:ty, |$r:ident| $($body:tt)*) => {
        $vis struct $name;

        impl $crate::RouteTable<$state> for $name {
            fn define<C: $crate::Collector>(
                $r: $crate::ApiRouter<$state, C>,
            ) -> $crate::ApiRouter<$state, C> {
                $crate::ApiRouter::from_parts($crate::IntoApiRouterAny::into_router_parts($($body)*))
            }
        }

        impl $name {
            /// Build the lean production server router (`NoCollect`).
            pub fn router() -> ::axum::Router<$state> {
                <$name as $crate::RouteTable<$state>>::router()
            }

            /// Collect TypeScript type data for binding generation (`TypeRegistry`).
            pub fn collect_types() -> $crate::RouteCollection {
                <$name as $crate::RouteTable<$state>>::collect_types()
            }

            /// Collecting build — returns both the `Router` and `RouteCollection`.
            pub fn build() -> (::axum::Router<$state>, $crate::RouteCollection) {
                <$name as $crate::RouteTable<$state>>::build()
            }
        }
    };
}

/// Declare an API's credential lifecycle as a marker type with a `flow()`
/// constructor for [`ApiRouter::auth_flow`](crate::ApiRouter::auth_flow).
///
/// One declaration collapses every place auth facts live server-side: the
/// routes that mint/renew the bearer (validated at `build()` — a lifecycle
/// route marked `Private` is the circular-marking bug), the WS ticket mint,
/// and any constants the client should have pinned (issuer, client id…).
/// The generated client turns the declaration into session lifecycle
/// machinery: a `TokenStore`, single-flight re-derivation on 401 with one
/// replay, and `onSessionExpired`.
///
/// The lifecycle is vocabulary-free — OAuth2 authorization-code flows and
/// lower-grade credential exchanges (session cookie → JWT) declare the same
/// three facts. Flow *logic* (PKCE, browser sessions, storage) stays in the
/// app; this macro carries facts.
///
/// Route fields take `register!(handler)` — the reference is resolved with
/// the same name transform the router applies, so a typo is a compile error
/// and an `.as_()` rename is caught at startup.
///
/// # Example
/// ```rust,ignore
/// axotyped::auth_scheme! {
///     pub SessionAuth {
///         kind: exchange,
///         // GET /auth/me — session cookie → JWT, anonymous is an answer.
///         exchange: register!(me),
///         refresh:   register!(me),
///     }
/// }
///
/// ApiRouter::<Arc<AppState>>::new()
///     .auth_flow(SessionAuth::flow())
///     .get("/auth/me", register!(me)) // #[endpoint(permissive)]
/// ```
#[macro_export]
macro_rules! auth_scheme {
    (
        $(#[$meta:meta])*
        $vis:vis $name:ident {
            kind: $kind:ident,
            $($fields:tt)*
        }
    ) => {
        $(#[$meta])*
        $vis struct $name;

        impl $name {
            /// The declared lifecycle — attach with
            /// [`ApiRouter::auth_flow`](crate::ApiRouter::auth_flow).
            pub fn flow() -> $crate::AuthFlow {
                $crate::__auth_scheme_munch!(@
                    [$crate::AuthFlow::new(stringify!($name), $crate::__auth_kind!($kind))]
                    $($fields)*
                )
            }
        }
    };
}

/// Internal field muncher for [`auth_scheme!`](crate::auth_scheme). Not part
/// of the public API.
#[doc(hidden)]
#[macro_export]
macro_rules! __auth_scheme_munch {
    // Terminal — emit the accumulated builder chain.
    (@ [$($acc:tt)*]) => { $($acc)* };
    (@ [$($acc:tt)*] ,) => { $($acc)* };
    // Field rules come in comma / trailing-comma-less pairs: an optional
    // `$(,)?` before `$($rest:tt)*` is ambiguous (rest can eat the comma),
    // so the separator is matched explicitly.
    (@ [$($acc:tt)*] exchange: register!($h:path) , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ [$($acc)* .bootstrap($h)] $($rest)*)
    };
    (@ [$($acc:tt)*] exchange: register!($h:path)) => {
        $($acc)* .bootstrap($h)
    };
    // `-> field`: declare the response field carrying the bearer; the
    // generated client binds its default refresh hook to this route.
    (@ [$($acc:tt)*] exchange: register!($h:path) -> $f:ident , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@
            [$($acc)* .bootstrap($h).token_field(stringify!($f))] $($rest)*)
    };
    (@ [$($acc:tt)*] exchange: register!($h:path) -> $f:ident) => {
        $($acc)* .bootstrap($h).token_field(stringify!($f))
    };
    (@ [$($acc:tt)*] refresh: register!($h:path) , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ [$($acc)* .refresh($h)] $($rest)*)
    };
    (@ [$($acc:tt)*] refresh: register!($h:path)) => {
        $($acc)* .refresh($h)
    };
    (@ [$($acc:tt)*] refresh: register!($h:path) -> $f:ident , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@
            [$($acc)* .refresh($h).token_field(stringify!($f))] $($rest)*)
    };
    (@ [$($acc:tt)*] refresh: register!($h:path) -> $f:ident) => {
        $($acc)* .refresh($h).token_field(stringify!($f))
    };
    (@ [$($acc:tt)*] ws_ticket: register!($h:path) , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ [$($acc)* .ws_ticket($h)] $($rest)*)
    };
    (@ [$($acc:tt)*] ws_ticket: register!($h:path)) => {
        $($acc)* .ws_ticket($h)
    };
    // OAuth2 pair form: `-> { access: a, refresh: r, expires_in: e }`
    // (refresh/expires_in optional, fixed member order). Members munch
    // separately; `access` is required so every path ends in a Pair decl.
    (@ [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident , refresh: $r:ident , expires_in: $e:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ [$($acc)* .bootstrap($h).token_pair(stringify!($a), Some(stringify!($r)), Some(stringify!($e)))] $($rest)*)
    };
    (@ [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident , refresh: $r:ident , expires_in: $e:ident }) => {
        $($acc)* .bootstrap($h).token_pair(stringify!($a), Some(stringify!($r)), Some(stringify!($e)))
    };
    (@ [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident , refresh: $r:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ [$($acc)* .bootstrap($h).token_pair(stringify!($a), Some(stringify!($r)), None)] $($rest)*)
    };
    (@ [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident , refresh: $r:ident }) => {
        $($acc)* .bootstrap($h).token_pair(stringify!($a), Some(stringify!($r)), None)
    };
    (@ [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident , expires_in: $e:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ [$($acc)* .bootstrap($h).token_pair(stringify!($a), None, Some(stringify!($e)))] $($rest)*)
    };
    (@ [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident , expires_in: $e:ident }) => {
        $($acc)* .bootstrap($h).token_pair(stringify!($a), None, Some(stringify!($e)))
    };
    (@ [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ [$($acc)* .bootstrap($h).token_pair(stringify!($a), None, None)] $($rest)*)
    };
    (@ [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident }) => {
        $($acc)* .bootstrap($h).token_pair(stringify!($a), None, None)
    };
    (@ [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident , refresh: $r:ident , expires_in: $e:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ [$($acc)* .refresh($h).token_pair(stringify!($a), Some(stringify!($r)), Some(stringify!($e)))] $($rest)*)
    };
    (@ [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident , refresh: $r:ident , expires_in: $e:ident }) => {
        $($acc)* .refresh($h).token_pair(stringify!($a), Some(stringify!($r)), Some(stringify!($e)))
    };
    (@ [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident , refresh: $r:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ [$($acc)* .refresh($h).token_pair(stringify!($a), Some(stringify!($r)), None)] $($rest)*)
    };
    (@ [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident , refresh: $r:ident }) => {
        $($acc)* .refresh($h).token_pair(stringify!($a), Some(stringify!($r)), None)
    };
    (@ [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident , expires_in: $e:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ [$($acc)* .refresh($h).token_pair(stringify!($a), None, Some(stringify!($e)))] $($rest)*)
    };
    (@ [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident , expires_in: $e:ident }) => {
        $($acc)* .refresh($h).token_pair(stringify!($a), None, Some(stringify!($e)))
    };
    (@ [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ [$($acc)* .refresh($h).token_pair(stringify!($a), None, None)] $($rest)*)
    };
    (@ [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident }) => {
        $($acc)* .refresh($h).token_pair(stringify!($a), None, None)
    };
    (@ [$($acc:tt)*] constant $key:ident : $value:expr , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ [$($acc)* .constant(stringify!($key), $value)] $($rest)*)
    };
    // `expr` fragments accept only `,`/`;`/`=>` after them, so the trailing
    // (comma-less) form terminates inline.
    (@ [$($acc:tt)*] constant $key:ident : $value:expr) => {
        $($acc)* .constant(stringify!($key), $value)
    };
}

/// Internal kind-label mapping for [`auth_scheme!`](crate::auth_scheme).
/// Not part of the public API.
#[doc(hidden)]
#[macro_export]
macro_rules! __auth_kind {
    (oauth2) => { $crate::AuthFlowKind::OAuth2 };
    (exchange) => { $crate::AuthFlowKind::Exchange };
    (static_bearer) => { $crate::AuthFlowKind::StaticBearer };
}

// ts integration: TS trait, derive macro, and all standard type impls.
// The derive macro lives in axotyped-macros
// and generates code referencing `::axotyped` paths.
mod ts;

// Re-export all ts-rs types so generated code can reference them as
// `::axotyped::TS`, `::axotyped::Config`, etc.
pub use ts::*;

// ---------------------------------------------------------------------------
// EndpointMeta — trait for auto-inferred route type metadata
// ---------------------------------------------------------------------------

/// Trait generated by `#[endpoint]` to carry inferred type metadata.
///
/// The proc-macro creates a companion struct (`<fn_name>__EndpointMeta`) that
/// implements this trait. The builder calls `apply()` when constructing the
/// route to populate the `RouteDefinition` with the extracted body/response/query types,
/// pushing each named type through the router's [`Collector`].
///
/// `apply` is generic over the collector so the lean (`NoCollect`) and collecting
/// (`TypeRegistry`) router builds monomorphize separately — keeping the ts-rs export
/// machinery out of binaries that don't collect types.
///
/// You should not implement this trait manually — use `#[endpoint]` on your
/// handler functions instead.
pub trait EndpointMeta: Sized {
    /// Type-level authorization fold: `Credential` when some parameter
    /// authenticates or the endpoint declared itself public/permissive,
    /// `OptionalCredential` otherwise. Set by `#[endpoint]`; the private-by-default
    /// chain bounds registration on it (see [`AllowsRoute`](crate::AllowsRoute)).
    type Authorization: crate::params::AuthFlag;

    /// The params-only credential fold (`Authorization` before the
    /// endpoint's own visibility declaration folds in). Drives the
    /// definition-site coherence check.
    type ParamCredentials: crate::params::AuthFlag;

    /// Apply inferred type metadata to a route definition and register named types
    /// for TypeScript export through `collector`.
    fn apply<C: Collector>(def: &mut RouteDefinition, collector: &mut C);
}

// ---------------------------------------------------------------------------
// __private — internals used by the #[endpoint] and register!() macros
// ---------------------------------------------------------------------------

/// Internal helpers used by generated code. Not part of the public API.
pub mod __private {
    pub use crate::builder::type_string;
}
