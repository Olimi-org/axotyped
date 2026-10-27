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

mod auth_flow;
mod params;

// Re-export public API
pub use auth_flow::{AuthFlow, AuthFlowKind, TokenFields, endpoint_name};
pub use generator::{
    AuthScheme, CheckError, GeneratorConfig, check, generate, generate_runtime_source,
    generate_to_file,
};
pub use params::{
    Allowed, AllowsRoute, AssertCredentiallessDeclaration, AuthFlag, AuthOr, Credential,
    EndpointParam, EndpointVisibilityCoherence, FlagConst, IsCoherentWithParams, LayeredAuthScope,
    NotAllowed, OptionalCredential, PermissiveScope, PrivateScope, PublicScope, WireFields,
    wire_field_declared,
};
pub use types::{
    Collector, HttpMethod, NoCollect, PathParam, RouteCollection, RouteDefinition, TypeRegistry,
    Visibility, WsBinaryMode, extract_path_params, is_valid_js_identifier,
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
#[doc(hidden)]
pub use axotyped_macros::check_response_fields as __check_response_fields;
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
/// The declaration names the routes that mint/renew the bearer, the WS
/// ticket mint, and any constants the client should pin. Route fields take
/// `register!(handler)` — resolved with the same name transform the router
/// applies.
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
                    p []
                    c [$crate::AuthFlow::new(stringify!($name), $crate::__auth_kind!($kind))]
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
    // Terminal.
    (@ p [$($pr:tt)*] c [$($acc:tt)*]) => { { $($pr)* $($acc)* } };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] ,) => { { $($pr)* $($acc)* } };
    // Comma-separated field rules; the separator is matched explicitly.
    (@ p [$($pr:tt)*] c [$($acc:tt)*] exchange: register!($h:path) , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ p [$($pr)*] c [$($acc)* .bootstrap($h)] $($rest)*)
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] exchange: register!($h:path)) => {
        { $($pr)* $($acc)* .bootstrap($h) }
    };
    // `-> field`: the response field carrying the bearer; the probe
    // requires the field on the handler's response type.
    (@ p [$($pr:tt)*] c [$($acc:tt)*] exchange: register!($h:path) -> $f:ident , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@
            p [$($pr)* $crate::__check_response_fields!($h $f);]
            c [$($acc)* .bootstrap($h).token_field(stringify!($f))]
            $($rest)*)
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] exchange: register!($h:path) -> $f:ident) => {
        { $($pr)* $crate::__check_response_fields!($h $f); $($acc)* .bootstrap($h).token_field(stringify!($f)) }
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] refresh: register!($h:path) , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ p [$($pr)*] c [$($acc)* .refresh($h)] $($rest)*)
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] refresh: register!($h:path)) => {
        { $($pr)* $($acc)* .refresh($h) }
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] refresh: register!($h:path) -> $f:ident , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@
            p [$($pr)* $crate::__check_response_fields!($h $f);]
            c [$($acc)* .refresh($h).token_field(stringify!($f))]
            $($rest)*)
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] refresh: register!($h:path) -> $f:ident) => {
        { $($pr)* $crate::__check_response_fields!($h $f); $($acc)* .refresh($h).token_field(stringify!($f)) }
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] ws_ticket: register!($h:path) , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ p [$($pr)*] c [$($acc)* .ws_ticket($h)] $($rest)*)
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] ws_ticket: register!($h:path)) => {
        { $($pr)* $($acc)* .ws_ticket($h) }
    };
    // OAuth2 pair form: `-> { access, refresh, expires_in }`
    // (refresh/expires_in optional).
    (@ p [$($pr:tt)*] c [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident , refresh: $r:ident , expires_in: $e:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@
            p [$($pr)* $crate::__check_response_fields!($h $a, $r, $e);]
            c [$($acc)* .bootstrap($h).token_pair(stringify!($a), Some(stringify!($r)), Some(stringify!($e)))]
            $($rest)*)
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident , refresh: $r:ident , expires_in: $e:ident }) => {
        { $($pr)* $crate::__check_response_fields!($h $a, $r, $e); $($acc)* .bootstrap($h).token_pair(stringify!($a), Some(stringify!($r)), Some(stringify!($e))) }
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident , refresh: $r:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@
            p [$($pr)* $crate::__check_response_fields!($h $a, $r);]
            c [$($acc)* .bootstrap($h).token_pair(stringify!($a), Some(stringify!($r)), None)]
            $($rest)*)
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident , refresh: $r:ident }) => {
        { $($pr)* $crate::__check_response_fields!($h $a, $r); $($acc)* .bootstrap($h).token_pair(stringify!($a), Some(stringify!($r)), None) }
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident , expires_in: $e:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@
            p [$($pr)* $crate::__check_response_fields!($h $a, $e);]
            c [$($acc)* .bootstrap($h).token_pair(stringify!($a), None, Some(stringify!($e)))]
            $($rest)*)
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident , expires_in: $e:ident }) => {
        { $($pr)* $crate::__check_response_fields!($h $a, $e); $($acc)* .bootstrap($h).token_pair(stringify!($a), None, Some(stringify!($e))) }
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@
            p [$($pr)* $crate::__check_response_fields!($h $a);]
            c [$($acc)* .bootstrap($h).token_pair(stringify!($a), None, None)]
            $($rest)*)
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] exchange: register!($h:path) -> { access: $a:ident }) => {
        { $($pr)* $crate::__check_response_fields!($h $a); $($acc)* .bootstrap($h).token_pair(stringify!($a), None, None) }
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident , refresh: $r:ident , expires_in: $e:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@
            p [$($pr)* $crate::__check_response_fields!($h $a, $r, $e);]
            c [$($acc)* .refresh($h).token_pair(stringify!($a), Some(stringify!($r)), Some(stringify!($e)))]
            $($rest)*)
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident , refresh: $r:ident , expires_in: $e:ident }) => {
        { $($pr)* $crate::__check_response_fields!($h $a, $r, $e); $($acc)* .refresh($h).token_pair(stringify!($a), Some(stringify!($r)), Some(stringify!($e))) }
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident , refresh: $r:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@
            p [$($pr)* $crate::__check_response_fields!($h $a, $r);]
            c [$($acc)* .refresh($h).token_pair(stringify!($a), Some(stringify!($r)), None)]
            $($rest)*)
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident , refresh: $r:ident }) => {
        { $($pr)* $crate::__check_response_fields!($h $a, $r); $($acc)* .refresh($h).token_pair(stringify!($a), Some(stringify!($r)), None) }
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident , expires_in: $e:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@
            p [$($pr)* $crate::__check_response_fields!($h $a, $e);]
            c [$($acc)* .refresh($h).token_pair(stringify!($a), None, Some(stringify!($e)))]
            $($rest)*)
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident , expires_in: $e:ident }) => {
        { $($pr)* $crate::__check_response_fields!($h $a, $e); $($acc)* .refresh($h).token_pair(stringify!($a), None, Some(stringify!($e))) }
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident } , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@
            p [$($pr)* $crate::__check_response_fields!($h $a);]
            c [$($acc)* .refresh($h).token_pair(stringify!($a), None, None)]
            $($rest)*)
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] refresh: register!($h:path) -> { access: $a:ident }) => {
        { $($pr)* $crate::__check_response_fields!($h $a); $($acc)* .refresh($h).token_pair(stringify!($a), None, None) }
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] constant $key:ident : $value:expr , $($rest:tt)*) => {
        $crate::__auth_scheme_munch!(@ p [$($pr)*] c [$($acc)* .constant(stringify!($key), $value)] $($rest)*)
    };
    (@ p [$($pr:tt)*] c [$($acc:tt)*] constant $key:ident : $value:expr) => {
        { $($pr)* $($acc)* .constant(stringify!($key), $value) }
    };
}

/// Internal kind-label mapping for [`auth_scheme!`](crate::auth_scheme).
/// Not part of the public API.
#[doc(hidden)]
#[macro_export]
macro_rules! __auth_kind {
    (oauth2) => {
        $crate::AuthFlowKind::OAuth2
    };
    (exchange) => {
        $crate::AuthFlowKind::Exchange
    };
    (static_bearer) => {
        $crate::AuthFlowKind::StaticBearer
    };
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
/// implements this trait.
pub trait EndpointMeta: Sized {
    /// Type-level authorization answer for the route.
    type Authorization: crate::params::AuthFlag;

    /// The params-only credential fold. Drives the definition-site check.
    type ParamCredentials: crate::params::AuthFlag;

    /// Apply inferred type metadata to a route definition.
    fn apply<C: Collector>(def: &mut RouteDefinition, collector: &mut C);
}

// ---------------------------------------------------------------------------
// __private — internals used by the #[endpoint] and register!() macros
// ---------------------------------------------------------------------------

/// Internal helpers used by generated code. Not part of the public API.
pub mod __private {
    pub use crate::builder::type_string;
}
