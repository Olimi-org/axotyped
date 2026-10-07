//! Compile-time credential classification of handler parameters.
//!
//! `#[endpoint]` requires every extractor in the signature to implement
//! [`EndpointParam`]. Axum's built-in extractors have blanket impls here
//! (`OptionalCredential` — infrastructure, not credentials); an app's identity
//! extractors declare [`Credential`]:
//!
//! ```rust,ignore
//! impl axotyped::EndpointParam for AuthUser {
//!     type Auth = axotyped::Credential; // unauthenticated → extractor fails
//! }
//! impl axotyped::EndpointParam for OptionalAuthUser {} // OptionalCredential (default)
//! ```
//!
//! The endpoint macro folds every parameter's flag (plus the endpoint's own
//! public/permissive declaration) into `EndpointMeta::Authorization` at the
//! type level. The router's private-by-default chain bounds registration on
//! the projection `Authorization::Ok: MaybeAllowed` — a forgotten extractor
//! is a *compile error at the registration call site*, while handlers that
//! never authenticate (registered only in public/permissive scopes) still
//! compile: their `Authorization` is simply not `MaybeAllowed`.
//!
//! All of this is plain associated-type algebra — stable Rust, no
//! `generic_const_exprs`, no conditional impls.

// --- Type-level credential flags -------------------------------------------

/// The `Ok` projection of an [`Credential`] flag — what the private chain's
/// registration bound demands (`Authorization::Ok = Allowed`).
pub struct Allowed;
pub struct NotAllowed;

/// Type-level credential flag. `Ok` projects to [`Allowed`] exactly when
/// this flag is [`Credential`].
pub trait AuthFlag {
    type Ok;
}

/// The parameter enforces authentication: a request without valid
/// credentials cannot reach the handler body through it.
pub struct Credential;
impl AuthFlag for Credential {
    type Ok = Allowed;
}

/// The parameter carries no credential requirement (infrastructure
/// extractors, optional-identity extractors).
pub struct OptionalCredential;
impl AuthFlag for OptionalCredential {
    type Ok = NotAllowed;
}

/// Flag → const, for definition-site coherence checks (concrete types only).
pub trait FlagConst {
    const IS_CREDENTIAL: bool;
}
impl FlagConst for Credential {
    const IS_CREDENTIAL: bool = true;
}
impl FlagConst for OptionalCredential {
    const IS_CREDENTIAL: bool = false;
}

/// Type-level `Credential || OptionalCredential` — the flag OR.
pub trait AuthOr<Rhs> {
    type Out: AuthFlag;
}
impl AuthOr<Credential> for Credential {
    type Out = Credential;
}
impl AuthOr<OptionalCredential> for Credential {
    type Out = Credential;
}
impl AuthOr<Credential> for OptionalCredential {
    type Out = Credential;
}
impl AuthOr<OptionalCredential> for OptionalCredential {
    type Out = OptionalCredential;
}

/// Definition-site coherence: a credentialless declaration (public or
/// permissive — the client sends no required credential) combined with a
/// parameter extractor that REQUIRES one is incoherent: the extractor would
/// reject the anonymous callers the declaration promises to serve.
/// `#[endpoint]` implements this marker with a where clause that fails to
/// compile at the offending endpoint.
pub struct AssertCredentiallessDeclaration<const COHERENT: bool>;
pub trait IsCoherentWithParams {}
impl IsCoherentWithParams for AssertCredentiallessDeclaration<true> {}
/// Implemented by `#[endpoint]` with a where clause that is unsatisfiable
/// (→ compile error at the endpoint) for the incoherent combination.
#[doc(hidden)]
pub trait EndpointVisibilityCoherence {}

// --- Parameter classification ----------------------------------------------

/// Whether an extractor parameter enforces authentication.
///
/// Identity extractors (`AuthUser`, `AdminUser`) set `type Auth = Credential`.
/// Extractors that return `None` for anonymous callers (`OptionalAuthUser`)
/// and infrastructure (`State`, `Json`, `Query`, …) leave the default
/// `OptionalCredential`.
pub trait EndpointParam {
    type Auth: AuthFlag;
}

macro_rules! anonymous_ok_param {
    ($($t:ty),* $(,)?) => {
        $(
            impl EndpointParam for $t {
                type Auth = OptionalCredential;
            }
        )*
    };
}

// Blanket impls: axum's built-in extractors are infrastructure.
anonymous_ok_param!(
    axum::http::HeaderMap,
    axum::extract::Request,
    axum::extract::RawQuery,
    axum::extract::MatchedPath,
    axum::extract::OriginalUri,
    axum::extract::ConnectInfo<std::net::SocketAddr>,
);
impl<T> EndpointParam for axum::extract::State<T> {
    type Auth = OptionalCredential;
}
impl<T> EndpointParam for axum::Json<T> {
    type Auth = OptionalCredential;
}
impl<T> EndpointParam for axum::extract::Query<T> {
    type Auth = OptionalCredential;
}
impl<T> EndpointParam for axum::extract::Path<T> {
    type Auth = OptionalCredential;
}
impl<T> EndpointParam for axum::extract::Extension<T> {
    type Auth = OptionalCredential;
}
impl<T> EndpointParam for Option<T> {
    type Auth = OptionalCredential;
}
impl EndpointParam for axum::body::Bytes {
    type Auth = OptionalCredential;
}
#[cfg(feature = "axum-ws")]
impl EndpointParam for axum::extract::ws::WebSocketUpgrade {
    type Auth = OptionalCredential;
}

// --- Scope type-state -------------------------------------------------------

/// Registration scopes. The router carries its current scope as a type
/// parameter; `AllowsRoute` decides, per scope, which handlers compile:
///
/// - [`PrivateScope`] (the default chain): the handler's `Authorization`
///   must be `MaybeAllowed` — deny-by-default, now enforced by rustc
///   instead of trusted.
/// - [`PublicScope`] / [`PermissiveScope`] (group closures): anything goes.
/// - [`LayeredAuthScope`] (`auth_layer` in scope): middleware enforces
///   authentication, so the extractor requirement is lifted — claiming
///   middleware coverage is explicit at the call site.
pub struct PrivateScope;
pub struct PublicScope;
pub struct PermissiveScope;
pub struct LayeredAuthScope;

/// Whether `EH` may register on this scope.
pub trait AllowsRoute<S, EH, T> {}

impl<S, EH, T> AllowsRoute<S, EH, T> for PublicScope {}
impl<S, EH, T> AllowsRoute<S, EH, T> for PermissiveScope {}
impl<S, EH, T> AllowsRoute<S, EH, T> for LayeredAuthScope {}
impl<S, EH, T> AllowsRoute<S, EH, T> for PrivateScope
where
    EH: crate::IntoEndpointHandler<S, T>,
    <<EH as crate::IntoEndpointHandler<S, T>>::Meta as crate::EndpointMeta>::Authorization: AuthFlag<
        Ok = Allowed,
    >,
{
}
