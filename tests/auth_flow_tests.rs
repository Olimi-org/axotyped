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
/// response.
#[axotyped::endpoint(permissive)]
async fn me() {}

#[axotyped::endpoint]
async fn issue_ticket(_auth: TestAuth) {}

#[axotyped::endpoint]
async fn private_thing(_auth: TestAuth) {}

#[derive(serde::Serialize, axotyped::TS)]
struct ClientEvent {
    kind: String,
}
#[derive(serde::Serialize, axotyped::TS)]
struct ServerEvent {
    kind: String,
}
#[axotyped::endpoint]
async fn voice(_auth: TestAuth) {}

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
#[should_panic(expected = "is Private")]
fn private_bootstrap_fails_build() {
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

/// Input-taking ticket mint.
#[derive(serde::Deserialize)]
#[derive(axotyped::TS)]
struct TicketScopeQuery {
    _scope: String,
}

#[axotyped::endpoint]
async fn ticket_with_query(
    _auth: TestAuth,
    axum::extract::Query(_q): axum::extract::Query<TicketScopeQuery>,
) {
}

#[test]
#[should_panic(expected = "cannot serve the handshake")]
fn input_taking_ws_ticket_fails_build() {
    let _ = axotyped::ApiRouter::<()>::new()
        .auth_flow(
            axotyped::AuthFlow::new("X", axotyped::AuthFlowKind::StaticBearer)
                .ws_ticket_named("ticketWithQuery"),
        )
        .get("/ws/ticket", axotyped::register!(ticket_with_query))
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

    // …a public mint is a credential pathway hole and fails fast. A
    // credential-requiring mint in a public group no longer compiles at
    // all (mirror rule), so the runtime case uses an anonymous mint.
    #[axotyped::endpoint]
    async fn issue_ticket_anon() {}
    let result = std::panic::catch_unwind(|| {
        let flow = axotyped::AuthFlow::new("T", axotyped::AuthFlowKind::StaticBearer)
            .ws_ticket_named("issueTicketAnon");
        let _ = axotyped::ApiRouter::<()>::new()
            .auth_flow(flow)
            .group_public("ws", |g| {
                g.post("/ticket", axotyped::register!(issue_ticket_anon))
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
fn chained_auth_flow_calls_fail() {
    let _ = axotyped::ApiRouter::<()>::new()
        .auth_flow(SessionAuth::flow())
        .auth_flow(StaticToken::flow());
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

/// Each side's flow was validated against its own routes only — the merged
/// table can collide names with the surviving flow's references. Extend
/// re-validates, so the union is held to the same unambiguity a single
/// router's `build()` enforces.
#[test]
#[should_panic(expected = "merged route table invalidates")]
fn extend_revalidates_flow_references_against_the_union() {
    let (_r1, c1) = axotyped::ApiRouter::<()>::new()
        .auth_flow(SessionAuth::flow())
        .get("/auth/me", axotyped::register!(me))
        .build();
    let (_r2, c2) = axotyped::ApiRouter::<()>::new()
        .group("other", |g| {
            g.get("/me", axotyped::register!(me_typed)).as_("me")
        })
        .build();
    let mut combined = c1;
    combined.extend(c2);
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

    // The declaration lands as data the application reads. Mechanism
    // assertions live in the executed behavioral spec, not here.
    assert!(out.contains("export const AUTH"), "strategy const");
    assert!(out.contains("kind: \"exchange\""), "kind label");
    assert!(out.contains("bootstrap: [\"me\"]"), "bootstrap refs");
    assert!(out.contains("\"issuer\": \"https://sso.example.com\""), "constant (quoted key)");
    assert!(out.contains("\"clientId\": \"client_123\""), "camelCased constant key, quoted");
    // The transport takes facts and a token source — no lifecycle bundle.
    assert!(out.contains("getToken?:"), "bearer source is named by the app");
    assert!(out.contains("onUnauthorized?:"), "renewal policy is the app's");
    // One bearer source per client: `getToken` is the only one.
    assert_eq!(
        out.matches("getToken?:").count(),
        1,
        "flow clients must carry exactly one bearer source: {out}"
    );
    // Synthetic auth namespace aliases the lifecycle routes.
    assert!(out.contains("\"auth\": {"), "auth namespace");
}

/// The renewal helper needs a response type to read fields off; a lifecycle
/// route returning `()` gets none.
#[derive(serde::Serialize, axotyped::TS)]
struct BearerResponse {
    token: String,
    expires_in: u64,
}

#[axotyped::endpoint(permissive)]
async fn exchange() -> axum::Json<BearerResponse> {
    axum::Json(BearerResponse {
        token: String::new(),
        expires_in: 0,
    })
}

#[test]
fn typed_renewal_helper_is_generated_from_declared_fields() {
    let flow = axotyped::AuthFlow::new("SessionAuth", axotyped::AuthFlowKind::Exchange)
        .bootstrap_named("exchange")
        .refresh_named("exchange")
        .token_field("token");
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(flow)
        .get("/auth/me", axotyped::register!(exchange))
        .build();

    let out = generate(&collection, &config());
    assert!(
        out.contains("export async function refreshBearer(client: ApiClient)"),
        "helper signature: {out}"
    );
    // The field is read off the imported response type, so the compiler
    // checks the declared name against what the client will parse.
    assert!(
        out.contains("const res = await client.exchange();"),
        "helper calls the typed route method: {out}"
    );
    assert!(out.contains("return res.token ?? null;"), "field read: {out}");
    // And the declaration still lands as data for app glue.
    assert!(out.contains("export const AUTH"), "strategy const");
    assert!(out.contains("field: \"token\""), "declared field kept: {out}");
}

/// A non-identifier wire field must not be emitted as dot access.
#[test]
fn renewal_helper_quotes_non_identifier_wire_fields() {
    let flow = axotyped::AuthFlow::new("SessionAuth", axotyped::AuthFlowKind::Exchange)
        .bootstrap_named("exchange")
        .refresh_named("exchange")
        .token_field("x-token");
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(flow)
        .get("/auth/me", axotyped::register!(exchange))
        .build();

    let out = generate(&collection, &config());
    assert!(
        out.contains("return res[\"x-token\"] ?? null;"),
        "non-identifier field uses bracket access: {out}"
    );
}

/// An oauth2 pair lifts all three declared fields, normalising the two
/// optional ones to null rather than leaving them undefined.
#[derive(serde::Serialize, axotyped::TS)]
struct PairResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<u32>,
}

#[axotyped::endpoint(permissive)]
async fn pair() -> axum::Json<PairResponse> {
    axum::Json(PairResponse {
        access_token: String::new(),
        refresh_token: None,
        expires_in: None,
    })
}

#[test]
fn oauth2_pair_helper_lifts_every_declared_field() {
    let flow = axotyped::AuthFlow::new("OAuthLogin", axotyped::AuthFlowKind::OAuth2)
        .bootstrap_named("pair")
        .refresh_named("pair")
        .token_pair("access_token", Some("refresh_token"), Some("expires_in"));
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(flow)
        .get("/oauth/token", axotyped::register!(pair))
        .build();

    let out = generate(&collection, &config());
    assert!(
        out.contains("export async function refreshSession(client: ApiClient)"),
        "pair helper signature: {out}"
    );
    assert!(out.contains("access: res.access_token,"), "access lift: {out}");
    assert!(
        out.contains("refresh: res.refresh_token ?? null,"),
        "refresh normalises absent to null: {out}"
    );
    assert!(
        out.contains("expiresIn: res.expires_in == null ? null : Number(res.expires_in),"),
        "expiresIn coerces to number: {out}"
    );
}

/// A pair with only `access` declared still emits the helper — the two
/// optional members become unconditional nulls, not undefined holes.
#[test]
fn oauth2_pair_helper_tolerates_access_only_declaration() {
    let flow = axotyped::AuthFlow::new("OAuthLogin", axotyped::AuthFlowKind::OAuth2)
        .bootstrap_named("pair")
        .refresh_named("pair")
        .token_pair("access_token", None, None);
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(flow)
        .get("/oauth/token", axotyped::register!(pair))
        .build();

    let out = generate(&collection, &config());
    assert!(
        out.contains("access: res.access_token,"),
        "access lift: {out}"
    );
    assert!(
        out.contains("refresh: null,"),
        "an undeclared member is a literal null: {out}"
    );
    assert!(
        out.contains("expiresIn: null,"),
        "an undeclared member is a literal null: {out}"
    );
}

#[test]
fn flow_ws_ticket_wires_the_handshake() {
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(StaticToken::flow())
        .post("/ws/ticket", axotyped::register!(issue_ticket))
        .build();

    let out = generate(&collection, &config());
    // The WS handshake mints via the flow-declared route: the wiring rides
    // TRANSPORT as an OPERATIONS key, resolved to the route's entry.
    let table = out
        .split("export const OPERATIONS = {")
        .nth(1)
        .unwrap()
        .split("} satisfies")
        .next()
        .unwrap();
    assert!(table.contains("\"/ws/ticket\""), "ticket op in table: {table}");
    // The fixture's ticket route is untyped: the handshake still extracts
    // `{ticket}` from the body, so the descriptor must parse JSON.
    assert!(
        table.contains("response: \"json\""),
        "ticket op parses JSON regardless of exported response type: {table}"
    );
    assert!(
        out.contains("wsTicket: \"issueTicket\""),
        "TRANSPORT carries the flow's ticket key"
    );
    // static_bearer declares no renewal: no lifecycle surface exists, so
    // getToken remains the bearer source — the one-source rule follows the
    // declaration, not the scheme.
    assert!(out.contains("getToken?"), "static flow keeps getToken: {out}");
    assert!(!out.contains("auth?:"), "no lifecycle surface on static_bearer");
}

#[test]
fn oauth2_without_declared_fields_still_emits_a_strategy() {
    // No `-> { ... }`: the declaration carries no field names, so no helper
    // can be generated — but the emitted AUTH must still satisfy the
    // runtime's type (fields optional).
    let flow = axotyped::AuthFlow::new("X", axotyped::AuthFlowKind::OAuth2)
        .bootstrap_named("me");
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(flow)
        .group_public("auth", |g| {
            g.get("/me", axotyped::register!(me))
        })
        .build();

    let out = generate(&collection, &config());
    assert!(out.contains("kind: \"oauth2\""), "kind present");
    assert!(!out.contains("fields:"), "no fields without a declaration");
}

#[test]
fn no_flow_keeps_plain_bearer_emission() {
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .get("/auth/me", axotyped::register!(me))
        .get("/private", axotyped::register!(private_thing))
        .build();

    let out = generate(&collection, &config());
    assert!(!out.contains("export const AUTH"), "no strategy without a flow");
    assert!(out.contains("getToken?"), "plain bearer token source");
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

// -- declared token fields (`-> field`) --------------------------------------

/// Response carrying the bearer — `token` is `Option<String>`: anonymous
/// answers `null`, and the helper maps that to "no token" for the app.
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

/// Body-taking refresh route.
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
    // The auto-hook is strategy data: which route to call and which field
    // carries the token. Execution is covered by the behavioral spec.
    assert!(out.contains("refresh: \"meTyped\""), "refresh route key");
    assert!(out.contains("field: \"token\""), "declared field");
    assert!(out.contains("expiryFromJwt: true"), "jwt-derived proactive expiry");
}

#[test]
#[should_panic(expected = "don't match kind")]
fn token_field_rejected_for_oauth2() {
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
    assert!(
        warnings.iter().any(|w| w.contains("onUnauthorized")),
        "warning must state the remedy: {warnings:?}"
    );
    // No renewal helper can be generated, so none is.
    assert!(!out.contains("field: \""), "no extraction field: {out}");
    assert!(
        !out.contains("export async function refreshBearer"),
        "no helper to bind: {out}"
    );
    assert!(out.contains("refresh: \"refreshWithBody\""), "mint key stays: {out}");
}

/// Path-parameterized refresh route.
#[axotyped::endpoint(permissive)]
async fn refresh_tenant(
    axum::extract::Path(_tenant): axum::extract::Path<String>,
) -> axum::Json<MeResponse> {
    axum::Json(MeResponse { authenticated: false, token: None })
}

#[test]
fn parameterized_lifecycle_route_warns_and_skips_hook() {
    let flow = axotyped::AuthFlow::new("X", axotyped::AuthFlowKind::Exchange)
        .bootstrap_named("refreshTenant")
        .token_field("token");

    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(flow)
        .get("/tenants/{tenant}/refresh", axotyped::register!(refresh_tenant))
        .get("/private", axotyped::register!(private_thing))
        .build();

    let (out, warnings) = axotyped::generate_with_warnings(&collection, &config());
    assert!(
        warnings.iter().any(|w| w.contains("path parameters")),
        "warnings: {warnings:?}"
    );
    assert!(!out.contains("field: \""), "no auto hook: {out}");
}


/// Query-taking refresh route.
#[derive(serde::Deserialize)]
#[derive(axotyped::TS)]
struct TenantQuery {
    _tenant: String,
}

#[axotyped::endpoint(permissive)]
async fn refresh_with_query(
    axum::extract::Query(_q): axum::extract::Query<TenantQuery>,
) -> axum::Json<MeResponse> {
    axum::Json(MeResponse { authenticated: false, token: None })
}

#[test]
fn query_taking_lifecycle_route_warns_and_skips_hook() {
    let flow = axotyped::AuthFlow::new("X", axotyped::AuthFlowKind::Exchange)
        .bootstrap_named("refreshWithQuery")
        .token_field("token");

    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(flow)
        .get("/auth/refresh", axotyped::register!(refresh_with_query))
        .get("/private", axotyped::register!(private_thing))
        .build();

    let (out, warnings) = axotyped::generate_with_warnings(&collection, &config());
    assert!(
        warnings.iter().any(|w| w.contains("query string")),
        "warnings: {warnings:?}"
    );
    assert!(!out.contains("field: \""), "no auto hook: {out}");
}

/// Route names are unique per group only — a lifecycle reference naming a
/// route that exists twice would silently bind renewal to the first match.
#[test]
#[should_panic(expected = "ambiguous")]
fn ambiguous_lifecycle_reference_fails_build() {
    let flow = axotyped::AuthFlow::new("X", axotyped::AuthFlowKind::Exchange)
        .bootstrap_named("mint");

    let _ = axotyped::ApiRouter::<()>::new()
        .auth_flow(flow)
        .group("a", |g| {
            g.get("/mint", axotyped::register!(me)).as_("mint")
        })
        .group("b", |g| {
            g.get("/other/mint", axotyped::register!(me_typed)).as_("mint")
        })
        .build();
}

/// The Comment-3 shape: a route inside the `auth` group sharing a name
/// with a flow-referenced route elsewhere. Same-name lifecycle references
/// are ambiguous, so the collision dies HERE — at build, with the
/// structural remedy — and can never reach the generated client's
/// `api.auth.*` namespace as a silent wrong alias.
#[test]
#[should_panic(expected = "ambiguous")]
fn auth_group_route_sharing_a_lifecycle_name_fails_build() {
    let flow = axotyped::AuthFlow::new("X", axotyped::AuthFlowKind::Exchange)
        .bootstrap_named("me");

    let _ = axotyped::ApiRouter::<()>::new()
        .auth_flow(flow)
        .get("/me", axotyped::register!(me))
        .group("auth", |g| {
            g.get("/other", axotyped::register!(me_typed)).as_("me")
        })
        .build();
}


// -- the compile-time field probe checks the wire, not Rust visibility -------

/// A response type whose fields are PRIVATE to its module: perfectly
/// serializable (the wire carries them), invisible to sibling modules.
/// Field-access probes cannot name `token` from the declaration's module;
/// `WireFields` — published by the derive sitting on the type itself — can.
mod sealed {
    use super::*;

    #[derive(serde::Serialize)]
    #[derive(axotyped::TS)]
    pub struct SealedTokenResponse {
        token: String,
    }

    #[axotyped::endpoint(permissive)]
    pub async fn sealed_mint() -> axum::Json<SealedTokenResponse> {
        axum::Json(SealedTokenResponse { token: String::new() })
    }
}

axotyped::auth_scheme! {
    pub SealedFlow {
        kind: exchange,
        exchange: register!(sealed::sealed_mint) -> token,
        refresh: register!(sealed::sealed_mint) -> token,
    }
}

#[test]
fn flow_field_probe_reaches_private_fields_across_modules() {
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(SealedFlow::flow())
        .get("/mint", axotyped::register!(sealed::sealed_mint))
        .get("/private", axotyped::register!(private_thing))
        .build();

    let out = generate(&collection, &config());
    assert!(
        out.contains("field: \"token\""),
        "the declaration names a real wire field; privacy must not block it: {out}"
    );
}

#[derive(serde::Serialize)]
#[derive(axotyped::TS)]
struct RenamedTokenResponse {
    #[ts(rename = "accessToken")]
    access: String,
}

#[axotyped::endpoint(permissive)]
async fn mint_renamed() -> axum::Json<RenamedTokenResponse> {
    axum::Json(RenamedTokenResponse { access: String::new() })
}

axotyped::auth_scheme! {
    pub RenamedFlow {
        kind: exchange,
        exchange: register!(mint_renamed) -> accessToken,
        refresh: register!(mint_renamed) -> accessToken,
    }
}

#[test]
fn flow_field_probe_checks_the_wire_name_not_the_rust_name() {
    // `accessToken` is not a Rust field (the field is `access`) — it is the
    // name the response carries on the wire, which is the name the runtime
    // extracts and therefore the name the declaration must pin.
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(RenamedFlow::flow())
        .get("/mint", axotyped::register!(mint_renamed))
        .get("/private", axotyped::register!(private_thing))
        .build();

    let out = generate(&collection, &config());
    assert!(out.contains("field: \"accessToken\""), "{out}");
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
        exchange: register!(exchange_tokens) -> { access: access_token, refresh: refresh_token, expires_in: expires_in },
        refresh: register!(exchange_tokens) -> { access: access_token, refresh: refresh_token, expires_in: expires_in },
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
    // Verbatim field names — they name response fields the engine extracts.
    assert!(out.contains("kind: \"oauth2\""), "kind");
    assert!(out.contains("access: \"access_token\""), "access field");
    assert!(out.contains("refresh: \"refresh_token\""), "rotating refresh field");
    assert!(out.contains("expiresIn: \"expires_in\""), "expiry field");
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


#[test]
fn credentialed_ws_without_a_flow_ticket_warns() {
    // No flow, no ticket: the warning names the declaration remedy —
    // there is no config-string path anymore (one mechanism: auth_scheme!).
    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .group("realtime", |g| {
            g.ws("/voice", axotyped::register!(voice))
                .events::<ClientEvent, ServerEvent>()
        })
        .build();

    let (_out, warnings) = axotyped::generate_with_warnings(
        &collection,
        &GeneratorConfig { auth_scheme: AuthScheme::Bearer, ..config() },
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("voice") && w.contains("ws_ticket: register!")),
        "must name the declaration remedy: {warnings:?}"
    );
}
/// With grouping disabled the renewal helper must call the route's TOP-LEVEL
/// method — the flat emitter puts every method at the root, so a group
/// segment would reference a property the client does not have.
#[test]
fn renewal_helper_calls_the_flat_method_when_groups_are_disabled() {
    let flow = axotyped::AuthFlow::new("X", axotyped::AuthFlowKind::Exchange)
        .bootstrap_named("meTyped")
        .token_field("token");

    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(flow)
        .group_public("auth", |g| {
            g.get("/me", axotyped::register!(me_typed))
        })
        .get("/private", axotyped::register!(private_thing))
        .build();

    let mut flat = config();
    flat.enable_groups = false;
    let out = generate(&collection, &flat);
    assert!(
        out.contains("await client.meTyped()"),
        "flat helper calls the top-level method: {out}"
    );
    assert!(!out.contains("client.auth."), "no group segment when flat: {out}");

    // And the grouped default keeps the nested call.
    let out = generate(&collection, &config());
    assert!(
        out.contains("await client.auth.meTyped()"),
        "grouped helper nests: {out}"
    );
}

/// Non-identifier group and route names in the renewal helper use bracket
/// access — `client."email-password"` is not valid TypeScript, but
/// `client["email-password"]` is, and both spellings must reach the same
/// method.
#[test]
fn renewal_helper_brackets_non_identifier_names() {
    let flow = axotyped::AuthFlow::new("X", axotyped::AuthFlowKind::Exchange)
        .bootstrap_named("x-refresh")
        .refresh_named("x-refresh")
        .token_field("token");

    let (_router, collection) = axotyped::ApiRouter::<()>::new()
        .auth_flow(flow)
        .group_public("email-password", |g| {
            g.get("/refresh", axotyped::register!(me_typed)).as_("x-refresh")
        })
        .get("/private", axotyped::register!(private_thing))
        .build();

    let out = generate(&collection, &config());
    assert!(
        out.contains("await client[\"email-password\"][\"x-refresh\"]();"),
        "expression-position access must bracket non-identifiers: {out}"
    );
}
