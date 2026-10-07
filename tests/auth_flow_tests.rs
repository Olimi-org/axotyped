//! `auth_scheme!` / `AuthFlow` integration: declaration → router attachment
//! → build()-time validation → generated client emission.

use axotyped::{AuthScheme, GeneratorConfig, generate};

// -- handlers --------------------------------------------------------------

// Test identity extractor — satisfies the private chain's compile-time
// authorization requirement (Credential).
#[derive(Clone, Copy)]
pub struct TestAuth;

impl axotyped::EndpointParam for TestAuth {
    type Auth = axotyped::Credential;
}

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for TestAuth {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(
        _parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(TestAuth)
    }
}

/// Session cookie → JWT exchange. Permissive: anonymous callers get a normal
/// response, and requiring a token would be circular (the token only exists
/// after this call).
#[axotyped::endpoint(permissive)]
async fn me() {}

#[axotyped::endpoint]
async fn issue_ticket(_auth: TestAuth) {}

#[axotyped::endpoint]
async fn private_thing(_auth: TestAuth) {}

// -- declarations ----------------------------------------------------------

axotyped::auth_scheme! {
    pub SessionAuth {
        kind: exchange,
        exchange: register!(me),
        refresh: register!(me),
    }
}

axotyped::auth_scheme! {
    pub StaticToken {
        kind: static_bearer,
        ws_ticket: register!(issue_ticket),
    }
}

axotyped::auth_scheme! {
    pub WithConstants {
        kind: exchange,
        exchange: register!(me),
        constant issuer: "https://sso.example.com",
        constant client_id: "client_123",
    }
}

// -- build()-time validation -----------------------------------------------

#[test]
fn macro_flow_attaches_and_collects() {
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(SessionAuth::flow())
        .get("/auth/me", axotyped::register!(me))
        .build();

    let flow = collection.auth_flow().expect("flow collected");
    assert_eq!(flow.name, "SessionAuth");
    assert_eq!(flow.kind, axotyped::AuthFlowKind::Exchange);
    assert_eq!(flow.bootstrap, vec!["me".to_string()]);
    assert_eq!(flow.refresh.as_deref(), Some("me"));
}

#[test]
#[should_panic(expected = "circular")]
fn private_bootstrap_fails_build() {
    // A Private bootstrap route is the circular-marking bug the runtime
    // validation exists to catch (the compile-time layer already refuses
    // an *unauthenticated* private registration; this is the flow-level
    // rule).
    let flow = axotyped::AuthFlow::new("SessionAuth", axotyped::AuthFlowKind::Exchange)
        .bootstrap_named("issueTicket");
    let _ = axotyped::ApiRouter::<()>::new()
        .auth_flow(flow)
        .post("/ws/ticket", axotyped::register!(issue_ticket))
        .build();
}

#[test]
#[should_panic(expected = "not registered")]
fn missing_reference_fails_build() {
    let _ = axotyped::ApiRouter::<()>::new()
        .auth_flow(SessionAuth::flow())
        .get("/somewhere/else", axotyped::register!(private_thing))
        .build();
}

#[test]
fn ws_ticket_must_be_private() {
    // Private mint (the default) passes…
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(StaticToken::flow())
        .post("/ws/ticket", axotyped::register!(issue_ticket))
        .build();
    assert_eq!(collection.auth_flow().unwrap().ws_ticket.as_deref(), Some("issueTicket"));

    // …a public mint is a credential pathway hole and fails fast.
    let result = std::panic::catch_unwind(|| {
        let _ = axotyped::ApiRouter::<()>::new()
            .auth_flow(StaticToken::flow())
            .group_public("ws", |g| {
                g.post("/ticket", axotyped::register!(issue_ticket))
            })
            .build();
    });
    assert!(result.is_err(), "public ws_ticket mint must fail build()");
}

#[test]
#[should_panic(expected = "one credential lifecycle")]
fn merging_two_flows_fails() {
    let a = axotyped::ApiRouter::<()>::new().auth_flow(SessionAuth::flow());
    let b = axotyped::ApiRouter::<()>::new().auth_flow(StaticToken::flow());
    let _ = a.merge(b);
}

#[test]
#[should_panic(expected = "one credential lifecycle")]
fn extending_collections_with_two_flows_fails() {
    let (_r1, c1) = axotyped::ApiRouter::<()>::new()
        .auth_flow(SessionAuth::flow())
        .get("/auth/me", axotyped::register!(me))
        .build();
    let (_r2, c2) = axotyped::ApiRouter::<()>::new()
        .auth_flow(StaticToken::flow())
        .post("/ws/ticket", axotyped::register!(issue_ticket))
        .build();
    let mut combined = c1;
    combined.extend(c2);
}

#[test]
fn extending_collection_adopts_flow_when_absent() {
    let (_r1, c1) = axotyped::ApiRouter::<()>::new()
        .auth_flow(SessionAuth::flow())
        .get("/auth/me", axotyped::register!(me))
        .build();
    let (_r2, c2) = axotyped::ApiRouter::<()>::new()
        .get("/private", axotyped::register!(private_thing))
        .build();
    let mut combined = c2;
    combined.extend(c1);
    assert_eq!(combined.auth_flow().unwrap().name, "SessionAuth");
}

// -- generator emission ----------------------------------------------------

fn config() -> GeneratorConfig {
    GeneratorConfig {
        auth_scheme: AuthScheme::Bearer,
        ..Default::default()
    }
}

fn flow_collection() -> axotyped::RouteCollection {
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(WithConstants::flow())
        .get("/auth/me", axotyped::register!(me))
        .get("/private", axotyped::register!(private_thing))
        .build();
    collection
}

#[test]
fn lifecycle_machinery_emitted_when_flow_declared() {
    let out = generate(&flow_collection(), &config());

    assert!(out.contains("export interface TokenStore"), "TokenStore");
    assert!(
        out.contains("export interface AuthLifecycleOptions"),
        "lifecycle options"
    );
    assert!(out.contains("auth?: AuthLifecycleOptions"), "options field");
    // Single-flight re-derivation + 401 replay.
    assert!(out.contains("refreshOnce"), "single-flight refresh");
    assert!(out.contains("retried: true"), "401 replay guard");
    assert!(out.contains("onSessionExpired"), "session-expiry event");
    // Declaration pinned client-side.
    assert!(out.contains("export const AUTH_FLOW"), "AUTH_FLOW const");
    assert!(out.contains("kind: \"exchange\""), "kind label");
    assert!(out.contains("bootstrap: [\"me\"]"), "bootstrap refs");
    assert!(out.contains("issuer: \"https://sso.example.com\""), "constant");
    assert!(out.contains("clientId: \"client_123\""), "camelCased constant key");
    // Synthetic auth namespace aliases the lifecycle routes.
    assert!(out.contains("\"auth\": {"), "auth namespace");
}

#[test]
fn ws_ticket_path_derived_from_flow_when_unset() {
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(StaticToken::flow())
        .post("/ws/ticket", axotyped::register!(issue_ticket))
        .build();

    let out = generate(&collection, &config());
    // The WS handshake mints via the flow-declared route without
    // GeneratorConfig::ws_ticket_path being set.
    assert!(out.contains("\"/ws/ticket\""), "ticket path from flow");
}

#[test]
fn no_flow_keeps_plain_bearer_emission() {
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .get("/auth/me", axotyped::register!(me))
        .get("/private", axotyped::register!(private_thing))
        .build();

    let out = generate(&collection, &config());
    assert!(!out.contains("TokenStore"));
    assert!(!out.contains("AUTH_FLOW"));
    assert!(!out.contains("rederiveOnce"));
    // Plain local alias keeps the existing getToken contract byte-compatible
    // in behavior.
    assert!(out.contains("const getToken = options.getToken;"));
}

#[test]
fn flow_under_non_bearer_scheme_warns() {
    let cfg = GeneratorConfig {
        auth_scheme: AuthScheme::Cookie,
        ..Default::default()
    };
    let (_out, warnings) = axotyped::generate_with_warnings(&flow_collection(), &cfg);
    assert!(
        warnings.iter().any(|w| w.contains("non-Bearer")),
        "warnings: {warnings:?}"
    );
}

// -- auto-bound refresh hook (`-> field`) -------------------------------------

/// Response carrying the bearer — `token` is `Option<String>`: anonymous
/// answers `null`, which maps to "session unrecoverable" → onSessionExpired.
#[derive(serde::Serialize)]
#[derive(axotyped::TS)]
struct MeResponse {
    authenticated: bool,
    token: Option<String>,
}

#[axotyped::endpoint(permissive)]
async fn me_typed() -> axum::Json<MeResponse> {
    axum::Json(MeResponse { authenticated: false, token: None })
}

#[derive(serde::Deserialize)]
#[derive(axotyped::TS)]
struct RefreshRequest {
    _refresh_token: String,
}

/// Body-taking refresh route — the default hook cannot supply a body.
#[axotyped::endpoint(permissive)]
async fn refresh_with_body(
    axum::extract::Json(_body): axum::extract::Json<RefreshRequest>,
) -> axum::Json<MeResponse> {
    axum::Json(MeResponse { authenticated: false, token: None })
}

axotyped::auth_scheme! {
    pub AutoRefresh {
        kind: exchange,
        exchange: register!(me_typed) -> token,
        refresh: register!(me_typed) -> token,
    }
}

#[test]
fn token_field_binds_default_refresh_hook() {
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(AutoRefresh::flow())
        .get("/auth/me", axotyped::register!(me_typed))
        .get("/private", axotyped::register!(private_thing))
        .build();

    let out = generate(&collection, &config());
    // The hook defaults to the declared route when the app supplies none.
    assert!(out.contains("options.auth?.refresh ??"));
    assert!(out.contains("await request<MeResponse>(\"/auth/me\""), "typed route call");
    assert!(out.contains("resp?.token ?? null"), "declared field extraction");
    assert!(out.contains("tokenFields: { access: \"token\" }"), "AUTH_FLOW tokenFields");
}

#[test]
#[should_panic(expected = "don't match kind")]
fn token_field_rejected_for_oauth2() {
    // Rotating refresh credentials don't fit the one-string store — the
    // declaration refuses at build() instead of emitting a hook that drops
    // the new refresh token on the floor.
    let flow = axotyped::AuthFlow::new("X", axotyped::AuthFlowKind::OAuth2)
        .bootstrap_named("me")
        .token_field("token");
    let _ = axotyped::ApiRouter::<()>::new()
        .auth_flow(flow)
        .get("/auth/me", axotyped::register!(me))
        .build();
}

#[test]
fn body_taking_lifecycle_route_warns_and_skips_hook() {
    let flow = axotyped::AuthFlow::new("X", axotyped::AuthFlowKind::Exchange)
        .bootstrap_named("refreshWithBody")
        .token_field("token");

    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(flow)
        .post("/auth/refresh", axotyped::register!(refresh_with_body))
        .get("/private", axotyped::register!(private_thing))
        .build();

    let (out, warnings) = axotyped::generate_with_warnings(&collection, &config());
    assert!(
        warnings.iter().any(|w| w.contains("request body")),
        "warnings: {warnings:?}"
    );
    assert!(!out.contains("options.auth?.refresh ??"), "no auto hook");
}


// -- oauth2 pair (`-> { access, refresh, expires_in }`) ----------------------

#[derive(serde::Serialize)]
#[derive(axotyped::TS)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u32>,
}

#[axotyped::endpoint]
async fn exchange_tokens() -> axum::Json<TokenResponse> {
    axum::Json(TokenResponse { access_token: String::new(), refresh_token: None, expires_in: None })
}

axotyped::auth_scheme! {
    pub OAuthLogin {
        kind: oauth2,
        exchange: register!(exchange_tokens) -> { access: access_token, refresh: refresh_token },
        refresh: register!(exchange_tokens) -> { access: access_token, refresh: refresh_token },
    }
}

#[test]
fn oauth2_pair_builds_and_emits_session_contract() {
    // Proxy routes are Public: no bearer exists when they run.
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .group_public("auth", |g| {
            g.post("/token", axotyped::register!(exchange_tokens))
        })
        .auth_flow(OAuthLogin::flow())
        .build();

    let out = generate(&collection, &config());
    assert!(out.contains("export interface OAuthSession"), "session type");
    assert!(out.contains("export interface SessionStore"), "session store");
    assert!(out.contains("decodeJwtExp"), "exp decode fallback");
    assert!(out.contains("prev?.refresh ?? null"), "rotation-agnostic keep");
    assert!(out.contains("options.auth?.store?.set(session)"), "single storage path");
    assert!(out.contains("s.expiresAt - 30_000"), "proactive skew");
    assert!(out.contains("tokenFields: { access: \"accessToken\", refresh: \"refreshToken\", expiresIn: null }"), "AUTH_FLOW pair");
    assert!(out.contains("store?: AuthStore"), "auth store contract");
}

// -- flow declared inside a group closure ------------------------------------

#[test]
fn flow_inside_group_is_adopted_not_dropped() {
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .group("auth", |g| {
            g.auth_flow(SessionAuth::flow())
                .get("/auth/me", axotyped::register!(me))
        })
        .build();
    let flow = collection.auth_flow().expect("inner flow adopted");
    assert_eq!(flow.name, "SessionAuth");
}

#[test]
#[should_panic(expected = "two auth flows declared")]
fn flow_conflict_between_root_and_group_fails_fast() {
    let _ = axotyped::ApiRouter::<()>::new()
        .auth_flow(SessionAuth::flow())
        .get("/auth/me", axotyped::register!(me))
        .group("extra", |g| {
            g.auth_flow(WithConstants::flow())
                .get("/x", axotyped::register!(private_thing))
        })
        .build();
}
