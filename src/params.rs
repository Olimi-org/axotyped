//! Compile-time credential classification of handler parameters.
//!
//! `#[endpoint]` requires every extractor in the signature to implement
//! [`EndpointParam`]. Axum's built-in extractors have blanket impls here
//! (`OptionalCredential` — infrastructure, not credentials); an app's identity
//! extractors declare [`Credential`]. The attribute macros write the impl for
//! you — prefer them:
//!
//! ```rust,ignore
//! #[axotyped::credential]
//! pub struct AuthUser(/* ... */);          // unauthenticated → extractor fails
//!
//! #[axotyped::optional_credential]
//! pub struct OptionalAuthUser(/* ... */);  // yields None for anonymous callers
//! ```
//!
//! Written out by hand, both associated types are named explicitly — stable
//! Rust has no associated-type defaults, so an empty impl does not compile:
//!
//! ```rust,ignore
//! impl axotyped::EndpointParam for AuthUser {
//!     type Auth = axotyped::Credential;
//! }
//! impl axotyped::EndpointParam for OptionalAuthUser {
//!     type Auth = axotyped::OptionalCredential;
//! }
//! ```
//!
//! The endpoint macro folds every parameter's flag (plus the endpoint's own
//! public/permissive declaration) into `EndpointMeta::Authorization` at the
//! type level. The router's private-by-default chain bounds registration on
//! `Authorization: AuthFlag` projecting to [`Allowed`] — a forgotten extractor
//! is a *compile error at the registration call site*, while handlers that
//! never authenticate (registered only in public/permissive scopes) still
//! compile: their `Authorization` projects to [`NotAllowed`].
//!
//! All of this is plain associated-type algebra — stable Rust, no
//! `generic_const_exprs`, no conditional impls.
//!
//! Name matching is path-aware: a bare `State` means axum's. A custom
//! extractor with the same bare name must use its qualified path
//! (`crate::auth::State`) to take part in credential checks.

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

/// Definition-site coherence: a `public`/`permissive` endpoint whose
/// parameter requires credentials does not compile.
/// `#[endpoint]` enforces this at the endpoint definition.
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
/// and infrastructure (`State`, `Json`, `Query`, …) set
/// `type Auth = OptionalCredential` — there is no default to lean on
/// (stable Rust has no associated-type defaults); `#[axotyped::credential]`
/// and `#[axotyped::optional_credential]` write the impl for you.
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

// Blanket impls: axum's built-in extractors are infrastructure. #[endpoint]
// asserts EndpointParam for EVERY parameter and the orphan rule bars
// downstream crates from adding impls for foreign types, so a missing entry
// here means that extractor cannot appear in an endpoint signature at all.
anonymous_ok_param!(
    axum::http::HeaderMap,
    axum::http::Method,
    axum::http::Uri,
    axum::http::Version,
    axum::body::Body,
    String,
    axum::extract::Request,
    axum::extract::RawQuery,
    axum::extract::MatchedPath,
    axum::extract::OriginalUri,
    axum::extract::NestedPath,
);
#[cfg(feature = "axum-multipart")]
impl EndpointParam for axum::extract::Multipart {
    type Auth = OptionalCredential;
}
impl<T> EndpointParam for axum::extract::ConnectInfo<T> {
    type Auth = OptionalCredential;
}
impl<T> EndpointParam for axum::extract::State<T> {
    type Auth = OptionalCredential;
}
impl<T> EndpointParam for axum::Form<T> {
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
/// - [`PrivateScope`] (default): the handler's `Authorization` must be
///   `MaybeAllowed`.
/// - [`PublicScope`] / [`PermissiveScope`]: parameters must not require
///   credentials (checked on `ParamCredentials`).
/// - [`LayeredAuthScope`]: the extractor requirement is lifted.
pub struct PrivateScope;
pub struct PublicScope;
pub struct PermissiveScope;

/// Scope entered via `auth_layer`. Not constructible: `new`, `Default`,
/// and `from_parts` accept initial scopes only.
///
/// ```rust,compile_fail
/// fn main() {
///     let _ = axotyped::ApiRouter::<(), axotyped::NoCollect, axotyped::LayeredAuthScope>::new();
/// }
/// ```
pub struct LayeredAuthScope;

/// Scopes accepted by `new`, `Default`, and `from_parts`.
#[diagnostic::on_unimplemented(
    message = "a router cannot be constructed in this scope",
    label = "start from the default private chain and enter scopes structurally: `group_public` / `group_permissive` / `.auth_layer(...)`",
    note = "`new()` starts the runtime scope at its private default and installs nothing; the scope type alone must not claim otherwise"
)]
pub trait InitialScope: initial_scope::Sealed {}
impl InitialScope for PrivateScope {}

mod initial_scope {
    pub trait Sealed {}
    impl Sealed for super::PrivateScope {}
}

/// Marker: a WebSocket route builder whose event types are declared.
pub struct EventsDeclared;
/// Marker: a WebSocket route builder with no event types yet.
pub struct NoEvents;

/// Finalization capability for WebSocket routes: builders must call
/// `events::<Send, Receive>()` before leaving the chain.
#[diagnostic::on_unimplemented(
    message = "a `[ws]` route must declare its event types before the chain continues",
    label = "call `.events::<Send, Receive>()` on this WebSocket route, or register the raw socket outside the axotyped route table",
    note = "an untyped WebSocket route has no client contract to generate"
)]
pub trait WsEventsComplete {}
impl WsEventsComplete for EventsDeclared {}

/// Whether `EH` may register on this scope.
pub trait AllowsRoute<S, EH, T> {}
impl<S, EH, T> AllowsRoute<S, EH, T> for PublicScope
where
    EH: crate::IntoEndpointHandler<S, T>,
    <<EH as crate::IntoEndpointHandler<S, T>>::Meta as crate::EndpointMeta>::ParamCredentials: AuthFlag<
        Ok = NotAllowed,
    >,
{
}
impl<S, EH, T> AllowsRoute<S, EH, T> for PermissiveScope
where
    EH: crate::IntoEndpointHandler<S, T>,
    <<EH as crate::IntoEndpointHandler<S, T>>::Meta as crate::EndpointMeta>::ParamCredentials: AuthFlag<
        Ok = NotAllowed,
    >,
{
}
impl<S, EH, T> AllowsRoute<S, EH, T> for LayeredAuthScope {}
impl<S, EH, T> AllowsRoute<S, EH, T> for PrivateScope
where
    EH: crate::IntoEndpointHandler<S, T>,
    <<EH as crate::IntoEndpointHandler<S, T>>::Meta as crate::EndpointMeta>::Authorization: AuthFlag<
        Ok = Allowed,
    >,
{
}

// ---------------------------------------------------------------------------
// Wire field names — compile-time checking of auth_scheme! declarations
// ---------------------------------------------------------------------------

/// The field names a type serializes to on the wire. Published by the
/// `TS` derive; `auth_scheme!` probes check declared fields against it.
#[diagnostic::on_unimplemented(
    message = "flow field declarations can only be checked on named structs deriving `axotyped::TS`",
    label = "this response type publishes no wire field names",
    note = "the TS derive emits `WireFields` for named structs; flattened or non-struct responses are not probed"
)]
pub trait WireFields {
    /// Field names as they appear in serialized JSON, renames honored.
    const WIRE_FIELDS: &'static [&'static str];
}

/// Whether `name` is among `fields`.
pub const fn wire_field_declared(fields: &'static [&'static str], name: &str) -> bool {
    let name = name.as_bytes();
    let mut i = 0;
    while i < fields.len() {
        let candidate = fields[i].as_bytes();
        if candidate.len() == name.len() {
            let mut j = 0;
            let mut equal = true;
            while j < candidate.len() {
                if candidate[j] != name[j] {
                    equal = false;
                    break;
                }
                j += 1;
            }
            if equal {
                return true;
            }
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod wire_fields_tests {
    use super::wire_field_declared;

    #[test]
    fn const_check_finds_declared_names_only() {
        const FIELDS: &[&str] = &["token", "expires_in"];
        assert!(wire_field_declared(FIELDS, "token"));
        assert!(wire_field_declared(FIELDS, "expires_in"));
        assert!(!wire_field_declared(FIELDS, "tokn"));
        assert!(!wire_field_declared(FIELDS, ""));
        assert!(!wire_field_declared(&[], "token"));
        // No prefix/suffix slips through: equal length is necessary.
        assert!(!wire_field_declared(FIELDS, "toke"));
    }
}
