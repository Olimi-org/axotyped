//! Server-side declaration of an API's credential lifecycle.
//!
//! An [`AuthFlow`] names the routes that mint and renew the bearer credential
//! so the generated client can own the session lifecycle (single-flight
//! re-derivation, 401 replay, session-expiry events) instead of every
//! consumer re-implementing it. The declaration is vocabulary-free: OAuth2
//! authorization-code flows and lower-grade credential exchanges (session
//! cookie → JWT) share the same three moves — bootstrap, expiry, re-derive.
//!
//! Declare with [`crate::auth_scheme!`] and attach to the router:
//!
//! ```rust,ignore
//! auth_scheme! {
//!     pub SessionAuth {
//!         kind: exchange,
//!         exchange: register!(me),   // cookie → JWT bootstrap
//!         refresh:   register!(me),
//!     }
//! }
//!
//! ApiRouter::<Arc<AppState>>::new()
//!     .auth_flow(SessionAuth::flow())
//!     .get("/auth/me", register!(me))
//! ```
//!
//! [`ApiRouter::build`](crate::ApiRouter::build) validates the declaration
//! against the registered routes and panics on mismatch: references must
//! resolve, `exchange`/`refresh` routes must not be `Private`, and the WS
//! ticket mint must be `Private`.

use crate::types::{RouteDefinition, Visibility};

/// The shape of the credential lifecycle a scheme declares. Emitted into
/// the client's `AUTH` constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFlowKind {
    /// Authorization-code (+ PKCE) OAuth2: an external issuer exchanges a
    /// code for tokens through backend proxy routes. Requires bootstrap.
    OAuth2,
    /// A lower-grade long-lived credential (session cookie, OpenID callback)
    /// is exchanged for a short-lived bearer. Requires bootstrap.
    Exchange,
    /// A static bearer with no lifecycle routes (API keys, mock tokens).
    /// Forbids bootstrap/refresh; only `ws_ticket` may be declared.
    StaticBearer,
}

impl AuthFlowKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::OAuth2 => "oauth2",
            Self::Exchange => "exchange",
            Self::StaticBearer => "static_bearer",
        }
    }
}

/// Response fields of a lifecycle route that carry the credential.
///
/// `Bearer` covers exchange-style flows. `Pair` covers OAuth2 token
/// responses: access plus optional refresh and `expires_in`.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenFields {
    /// Single field carrying the bearer.
    Bearer(String),
    /// OAuth2 pair: access (required), refresh and `expires_in` (optional).
    Pair {
        access: String,
        refresh: Option<String>,
        expires_in: Option<String>,
    },
}

/// A declared credential lifecycle, attached via
/// [`ApiRouter::auth_flow`](crate::ApiRouter::auth_flow).
///
/// Route references are *client method names* — the same names the generator
/// derives from handler functions — resolved and validated against the route
/// table at `build()` time. The [`crate::auth_scheme!`] macro derives them
/// from handler paths so a typo is a compile error and a rename is caught at
/// startup.
#[derive(Debug, Clone)]
pub struct AuthFlow {
    /// Scheme name (for diagnostics and the emitted `AUTH_FLOW` constant).
    pub name: String,
    /// The lifecycle shape.
    pub kind: AuthFlowKind,
    /// Opaque facts the app wants pinned client-side (issuer, client id,
    /// scopes…). Emitted verbatim into `AUTH_FLOW.constants`.
    pub constants: Vec<(String, String)>,
    /// Client method names of routes that mint a bearer from a lower-grade
    /// credential. Must not be `Private`.
    pub bootstrap: Vec<String>,
    /// Client method name of the route that renews a bearer. Must not be
    /// `Private`. May be the same route as a bootstrap entry.
    pub refresh: Option<String>,
    /// Response fields carrying the credential on the lifecycle routes
    /// (`-> token` for exchange kinds, `-> { access, ... }` for oauth2).
    /// When set, the generated client emits a typed renewal helper that
    /// reads these fields off the lifecycle route's response type.
    pub token_fields: Option<TokenFields>,
    /// Client method name of the WS ticket mint. Must be `Private`.
    pub ws_ticket: Option<String>,
}

/// Derive the client method name a handler will be registered under, using
/// the same transform as the router (`snake_case` fn → `camelCase`). Called
/// by [`crate::auth_scheme!`], public for the no-macro path.
pub fn endpoint_name<H: 'static>(_handler: H) -> String {
    crate::builder::default_name_from_handler::<H>()
}

impl AuthFlow {
    pub fn new(name: &str, kind: AuthFlowKind) -> Self {
        Self {
            name: name.to_string(),
            kind,
            constants: Vec::new(),
            bootstrap: Vec::new(),
            refresh: None,
            token_fields: None,
            ws_ticket: None,
        }
    }

    /// Reference a bootstrap route by its handler. The name is derived with
    /// the same transform the router applies, so the reference tracks the
    /// handler unless the route is renamed with `.as_()`.
    pub fn bootstrap<H: 'static>(mut self, handler: H) -> Self {
        self.bootstrap.push(endpoint_name(handler));
        self
    }

    /// Reference a bootstrap route by explicit client method name (for
    /// routes renamed with `.as_()`).
    pub fn bootstrap_named(mut self, name: &str) -> Self {
        self.bootstrap.push(name.to_string());
        self
    }

    /// Reference the refresh route by its handler.
    pub fn refresh<H: 'static>(mut self, handler: H) -> Self {
        self.refresh = Some(endpoint_name(handler));
        self
    }

    /// Reference the refresh route by explicit client method name.
    pub fn refresh_named(mut self, name: &str) -> Self {
        self.refresh = Some(name.to_string());
        self
    }

    /// Reference the WS ticket mint by its handler.
    pub fn ws_ticket<H: 'static>(mut self, handler: H) -> Self {
        self.ws_ticket = Some(endpoint_name(handler));
        self
    }

    /// Reference the WS ticket mint by explicit client method name.
    pub fn ws_ticket_named(mut self, name: &str) -> Self {
        self.ws_ticket = Some(name.to_string());
        self
    }

    /// Declare the single response field carrying the bearer (`-> token`).
    /// Exchange-kind. Panics on conflicting declarations.
    pub fn token_field(mut self, field: &str) -> Self {
        self.declare(TokenFields::Bearer(field.to_string()));
        self
    }

    /// Declare the OAuth2 pair fields
    /// (`-> { access, refresh, expires_in }`; refresh/expires_in optional).
    /// OAuth2-kind. Panics on conflicting declarations.
    pub fn token_pair(
        mut self,
        access: &str,
        refresh: Option<&str>,
        expires_in: Option<&str>,
    ) -> Self {
        self.declare(TokenFields::Pair {
            access: access.to_string(),
            refresh: refresh.map(str::to_string),
            expires_in: expires_in.map(str::to_string),
        });
        self
    }

    fn declare(&mut self, fields: TokenFields) {
        if let Some(existing) = &self.token_fields {
            if existing != &fields {
                panic!(
                    "auth flow '{}' declares conflicting token fields ({existing:?} vs {fields:?})",
                    self.name
                );
            }
        }
        self.token_fields = Some(fields);
    }

    /// Pin a client-visible constant (emitted into `AUTH_FLOW.constants`).
    pub fn constant(mut self, key: &str, value: &str) -> Self {
        self.constants.push((key.to_string(), value.to_string()));
        self
    }

    /// Validate the declaration against a route table.
    pub(crate) fn validate(&self, routes: &[RouteDefinition]) -> Result<(), String> {
        match self.kind {
            AuthFlowKind::StaticBearer if !self.bootstrap.is_empty() || self.refresh.is_some() => {
                return Err(
                    "kind is static_bearer but bootstrap/refresh routes are declared; a \
                     static credential has no lifecycle. Use kind exchange or oauth2."
                        .into(),
                );
            }
            AuthFlowKind::OAuth2 | AuthFlowKind::Exchange if self.bootstrap.is_empty() => {
                return Err(
                    "kind requires at least one exchange/bootstrap route (none declared).".into(),
                );
            }
            _ => {}
        }

        if let Some(fields) = &self.token_fields {
            let mismatch = |want: &str| {
                format!(
                    "token fields {fields:?} don't match kind '{}': {want}",
                    self.kind.label()
                )
            };
            match (&self.kind, fields) {
                (AuthFlowKind::Exchange, TokenFields::Bearer(_)) => {}
                (AuthFlowKind::OAuth2, TokenFields::Pair { .. }) => {}
                (AuthFlowKind::Exchange, _) => {
                    return Err(mismatch("use `-> field` (one bearer field)"));
                }
                (AuthFlowKind::OAuth2, _) => {
                    return Err(mismatch(
                        "use `-> { access, refresh, expires_in }` (pair; members optional)",
                    ));
                }
                (AuthFlowKind::StaticBearer, _) => {
                    return Err(mismatch("a static credential declares no token fields"));
                }
            }
            if self.refresh.is_none() && self.bootstrap.is_empty() {
                return Err("token fields declared but no lifecycle route to bind them to.".into());
            }
        }

        let check = |name: &str, role: &str, must_be_private: bool| -> Result<(), String> {
            let mut matches: Vec<&RouteDefinition> =
                routes.iter().filter(|r| r.name == name).collect();
            if matches.len() > 1 {
                let where_at: Vec<String> = matches
                    .iter()
                    .map(|r| {
                        format!(
                            "{} {} (group '{}')",
                            r.method,
                            r.path,
                            r.group.as_deref().unwrap_or("<root>")
                        )
                    })
                    .collect();
                return Err(format!(
                    "{role} route '{name}' is ambiguous — it matches multiple routes: \
                     [{}]. Rename the others with .as_().",
                    where_at.join(", ")
                ));
            }
            let Some(route) = matches.pop() else {
                return Err(format!(
                    "{role} route '{name}' is not registered on this router. If it was \
                     renamed with .as_(), reference the client method name via \
                     bootstrap_named/refresh_named/ws_ticket_named.",
                ));
            };
            if must_be_private {
                if route.visibility != Visibility::Private {
                    return Err(format!(
                        "{role} route '{name}' ({} {}) must be Private.",
                        route.method, route.path
                    ));
                }
                // A socket never has a descriptor in the client — the
                // generator skips any [ws] route without both event types,
                // since there is no contract to emit. Naming one as the
                // ticket mint would leave `TRANSPORT.wsTicket` pointing at a
                // key `OPERATIONS` never emits, and the handshake would fail
                // at connect time with nothing to act on.
                if route.websocket
                    && (route.ws_send_type.is_none() || route.ws_receive_type.is_none())
                {
                    return Err(format!(
                        "{role} route '{name}' ({} {}) is a WebSocket route without declared \
                         event types, so no client descriptor is generated for it. Declare \
                         `.events::<Send, Receive>()` on the route, or point {role} at a \
                         separate HTTP mint.",
                        route.method, route.path
                    ));
                }
                if route.body_type.is_some()
                    || route.query_type.is_some()
                    || !route.path_params.is_empty()
                {
                    return Err(format!(
                        "{role} route '{name}' ({} {}) takes a body, query, or path \
                         parameters and cannot serve the handshake.",
                        route.method, route.path
                    ));
                }
            } else if route.visibility == Visibility::Private {
                return Err(format!(
                    "{role} route '{name}' ({} {}) is Private. Lifecycle routes mint \
                     the credential; register it public or permissive.",
                    route.method, route.path
                ));
            }
            Ok(())
        };

        for name in &self.bootstrap {
            check(name, "bootstrap", false)?;
        }
        if let Some(name) = &self.refresh {
            check(name, "refresh", false)?;
        }
        if let Some(name) = &self.ws_ticket {
            check(name, "ws_ticket", true)?;
        }

        if self
            .bootstrap
            .iter()
            .any(|b| self.bootstrap.iter().filter(|x| *x == b).count() > 1)
        {
            return Err("duplicate bootstrap route references".into());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::HttpMethod;

    fn route(name: &str, visibility: Visibility) -> RouteDefinition {
        RouteDefinition {
            name: name.into(),
            method: HttpMethod::Get,
            path: format!("/{name}"),
            visibility,
            declared: visibility,
            body_type: None,
            response_type: None,
            query_type: None,
            path_params: vec![],
            group: None,
            allow_redirects: false,
            redirect: false,
            websocket: false,
            ws_send_type: None,
            ws_receive_type: None,
            ws_binary: crate::types::WsBinaryMode::None,
        }
    }

    #[test]
    fn valid_exchange_flow() {
        let flow = AuthFlow::new("SessionAuth", AuthFlowKind::Exchange)
            .bootstrap_named("me")
            .refresh_named("me");
        let routes = vec![route("me", Visibility::Permissive)];
        assert!(flow.validate(&routes).is_ok());
    }

    #[test]
    fn missing_route_reference_fails() {
        let flow = AuthFlow::new("SessionAuth", AuthFlowKind::Exchange).bootstrap_named("gone");
        let err = flow.validate(&[]).unwrap_err();
        assert!(err.contains("not registered"), "{err}");
    }

    #[test]
    fn private_bootstrap_fails() {
        let flow = AuthFlow::new("SessionAuth", AuthFlowKind::Exchange).bootstrap_named("me");
        let routes = vec![route("me", Visibility::Private)];
        let err = flow.validate(&routes).unwrap_err();
        assert!(err.contains("is Private"), "{err}");
    }

    #[test]
    fn non_private_ws_ticket_fails() {
        let flow =
            AuthFlow::new("StaticToken", AuthFlowKind::StaticBearer).ws_ticket_named("issueTicket");
        let routes = vec![route("issueTicket", Visibility::Public)];
        let err = flow.validate(&routes).unwrap_err();
        assert!(err.contains("must be Private"), "{err}");
    }

    #[test]
    fn static_bearer_forbids_bootstrap() {
        let flow = AuthFlow::new("StaticToken", AuthFlowKind::StaticBearer).bootstrap_named("me");
        let err = flow.validate(&[]).unwrap_err();
        assert!(err.contains("static_bearer"), "{err}");
    }

    #[test]
    fn exchange_requires_bootstrap() {
        let flow = AuthFlow::new("StaticToken", AuthFlowKind::Exchange);
        let err = flow.validate(&[]).unwrap_err();
        assert!(err.contains("at least one"), "{err}");
    }

    #[test]
    fn endpoint_name_matches_router_derivation() {
        // Same transform the builder applies to handler fn items.
        fn ws_ticket_issue() {}
        assert_eq!(endpoint_name(ws_ticket_issue), "wsTicketIssue");
    }

    fn socket(events: bool) -> RouteDefinition {
        let mut r = route("socket", Visibility::Private);
        r.path = "/ws".into();
        r.websocket = true;
        r.ws_send_type = events.then(|| "ClientEvent".into());
        r.ws_receive_type = events.then(|| "ServerEvent".into());
        r
    }

    /// The generator emits no descriptor for a socket without both event
    /// types, so `TRANSPORT.wsTicket` would name a key `OPERATIONS` never
    /// writes — the handshake would resolve to nothing at connect time.
    #[test]
    fn ws_ticket_naming_an_untyped_socket_fails() {
        let flow =
            AuthFlow::new("StaticToken", AuthFlowKind::StaticBearer).ws_ticket_named("socket");
        let err = flow.validate(&[socket(false)]).unwrap_err();
        assert!(err.contains("event types"), "{err}");
    }

    #[test]
    fn ws_ticket_naming_a_typed_socket_passes() {
        let flow =
            AuthFlow::new("StaticToken", AuthFlowKind::StaticBearer).ws_ticket_named("socket");
        assert!(flow.validate(&[socket(true)]).is_ok());
    }
}
