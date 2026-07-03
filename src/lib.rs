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
//!     .group_with("admin", |g| {
//!         g.auth_all()
//!          .get("/projects", register!(list_projects))
//!              .done()
//!     })
//!     .build();
//! ```

mod generator;
mod types;
mod builder;
#[macro_use]
mod macros;

// Re-export public API
pub use generator::{CheckError, GeneratorConfig, check, generate, generate_to_file};
pub use types::{
    Collector, HttpMethod, NoCollect, PathParam, RouteCollection, RouteDefinition, TypeRegistry,
    extract_path_params,
};

pub use builder::{
    ApiRouter, IntoEndpointHandler, MaybeTs, Registered, RouteBuilder, WsRouteBuilder, build_routes,
    build_typed, collect_routes,
};

/// [`ApiRouter`] with [`TypeRegistry`] as the collector — the variant that collects route types
/// for ts-rs export.
///
/// Convenience alias for codegen entry points: `TypedApiRouter::<AppState>::new()` is equivalent
/// to `ApiRouter::<AppState, TypeRegistry>::new()`, and the collector type `C` can be left to
/// inference when passing the builder to a generic function.
#[cfg(feature = "ts-rs")]
pub type TypedApiRouter<S = ()> = ApiRouter<S, TypeRegistry>;

// Re-export the #[endpoint] attribute macro and register!() call-site macro.
pub use axotyped_macros::{endpoint, register};

/// Declare a route table as a zero-sized type with `router()` and `collection()` methods.
///
/// The consumer never names or imports the [`Collector`] mechanism, [`ApiRouter`], [`NoCollect`],
/// or [`TypeRegistry`] — the macro expands to a unit struct whose inherent methods do the lean
/// vs. collecting build internally via [`build_routes`] / [`collect_routes`].
///
/// - `Routes::router()` → `axum::Router<S>` (lean — no binding-gen code in the binary)
/// - `Routes::collection()` → [`RouteCollection`] (for codegen; call from a debug-only entry point)
///
/// # Example
/// ```rust,ignore
/// axotyped::define_routes! {
///     pub Routes for Arc<AppState> {
///         r.get("/health", axotyped::register!(health)).done()
///             .group_with("admin", |g| g.auth_all().post("/x", axotyped::register!(create_x)).done())
///     }
/// }
///
/// let router = Routes::router().with_state(state);
/// let collection = Routes::collection(); // from a debug-only codegen entry point
/// ```
#[cfg(feature = "ts-rs")]
#[macro_export]
macro_rules! define_routes {
    ($vis:vis $name:ident for $state:ty, |$r:ident| $($body:tt)*) => {
        $vis struct $name;

        impl $name {
            /// Build the lean server router (no type collection — the ts-rs export machinery stays
            /// out of the binary). Apply `.with_state(state)` and any middleware at the call site.
            pub fn router() -> ::axum::Router<$state> {
                $crate::build_routes(|$r| $($body)*)
            }

            /// Collect the TypeScript type data for binding generation, returning the full
            /// [`RouteCollection`] (route metadata + collected types).
            ///
            /// Call this only from a debug-only codegen entry point so the collecting
            /// monomorphization is dead-code-eliminated from release builds.
            pub fn collect_types() -> $crate::RouteCollection {
                $crate::collect_routes(|$r| $($body)*)
            }

            /// Collecting build — returns both the `Router` and the collected `RouteCollection`
            /// from a single pass. Use when you need the router *and* the route types together;
            /// prefer [`router()`][Self::router] for the production server (which stays lean).
            pub fn build() -> (::axum::Router<$state>, $crate::RouteCollection) {
                $crate::build_typed(|$r| $($body)*)
            }
        }
    };
}

/// Without the `ts-rs` feature, [`define_routes!`] exposes only `router()` (no binding collection).
#[cfg(not(feature = "ts-rs"))]
#[macro_export]
macro_rules! define_routes {
    ($vis:vis $name:ident for $state:ty, |$r:ident| $($body:tt)*) => {
        $vis struct $name;

        impl $name {
            /// Build the server router. (Binding collection requires the `ts-rs` feature.)
            pub fn router() -> ::axum::Router<$state> {
                $crate::build_routes(|$r| $($body)*)
            }
        }
    };
}

// ts-rs integration: TS trait, derive macro, and all standard type impls.
// Gated behind the `ts-rs` feature. The derive macro lives in axotyped-macros
// and generates code referencing `::axotyped` paths.
#[cfg(feature = "ts-rs")]
mod ts;

// Re-export all ts-rs types so generated code can reference them as
// `::axotyped::TS`, `::axotyped::Config`, etc.
#[cfg(feature = "ts-rs")]
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
