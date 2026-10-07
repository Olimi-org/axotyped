use axotyped::{ApiRouter, HttpMethod, Visibility, register};
use axum::extract::{Json, Path, State};
use serde::Deserialize;

#[derive(Clone, Default)]
struct AppState;

#[derive(serde::Serialize)]
#[derive(axotyped::TS)]
struct UserResponse {
    _id: String,
}

#[derive(Deserialize)]
#[derive(axotyped::TS)]
struct CreateUserRequest {
    _name: String,
}

#[derive(Deserialize)]
#[derive(axotyped::TS)]
struct ListQuery {
    _limit: Option<u32>,
}

// Handler functions representing realistic endpoints

// Test identity extractor — satisfies the private chain's compile-time
// authorization requirement (Enforced), extracting as a no-op.
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

#[axotyped::endpoint]
async fn list_users(_auth: TestAuth, State(_state): State<AppState>,
    axum::extract::Query(_query): axum::extract::Query<ListQuery>) -> Json<Vec<UserResponse>> {
    Json(vec![])
}

#[axotyped::endpoint]
async fn get_user(_auth: TestAuth, State(_state): State<AppState>, Path(_id): Path<String>) -> Json<UserResponse> {
    Json(UserResponse { _id: "1".into() })
}

#[axotyped::endpoint]
async fn create_user(_auth: TestAuth, State(_state): State<AppState>,
    Json(_body): Json<CreateUserRequest>) -> Json<UserResponse> {
    Json(UserResponse { _id: "1".into() })
}

#[axotyped::endpoint]
async fn delete_user(_auth: TestAuth, State(_state): State<AppState>, Path(_id): Path<String>) {}

// ---------------------------------------------------------------------------
// Auto-naming: handler function name → camelCase
// ---------------------------------------------------------------------------

#[test]
fn auto_name_from_handler() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .get("/users", register!(list_users))
        .response::<Vec<UserResponse>>()
        .build();

    // list_users → listUsers
    assert_eq!(routes.routes()[0].name, "listUsers");
}

#[test]
fn auto_name_single_word() {
    #[axotyped::endpoint]
async fn register(_auth: TestAuth, State(_s): State<AppState>, Json(_b): Json<CreateUserRequest>) {}

    let (_router, routes) = ApiRouter::<AppState>::new()
        .post("/register", register!(register))
        .body::<CreateUserRequest>()
        .build();

    assert_eq!(routes.routes()[0].name, "register");
}

#[test]
fn as_overrides_auto_name() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .get("/users/{id}", register!(get_user))
        .response::<UserResponse>()
        .as_("getById")
        .build();

    assert_eq!(routes.routes()[0].name, "getById");
}

// ---------------------------------------------------------------------------
// post_json / put_json / patch_json shorthand
// ---------------------------------------------------------------------------

#[test]
fn json_shorthand() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .post("/users", register!(create_user))
        .json::<CreateUserRequest, UserResponse>()
        .build();

    let r = &routes.routes()[0];
    assert_eq!(r.name, "createUser");
    assert_eq!(r.method, HttpMethod::Post);
    assert!(r.is_credentialed());
    assert!(r.body_type.as_ref().unwrap().contains("CreateUserRequest"));
    assert!(r.response_type.as_ref().unwrap().contains("UserResponse"));
}

#[test]
fn json_shorthand_put() {
    #[axotyped::endpoint]
async fn update_user(_auth: TestAuth, State(_s): State<AppState>,
        Path(_id): Path<String>,
        Json(_b): Json<CreateUserRequest>) -> Json<UserResponse> {
        Json(UserResponse { _id: "1".into() })
    }

    let (_router, routes) = ApiRouter::<AppState>::new()
        .put("/users/{id}", register!(update_user))
        .json::<CreateUserRequest, UserResponse>()
        .build();

    let r = &routes.routes()[0];
    assert_eq!(r.name, "updateUser"); // auto snake→camel
    assert_eq!(r.method, HttpMethod::Put);
    assert!(r.body_type.as_ref().unwrap().contains("CreateUserRequest"));
    assert!(r.response_type.as_ref().unwrap().contains("UserResponse"));
    assert_eq!(r.path_params[0].name, "id");
}

// ---------------------------------------------------------------------------
// Full builder test (combines all features)
// ---------------------------------------------------------------------------

#[test]
fn builder_full_api() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .group("users", |g| {
            g.get("/users", register!(list_users))
                .response::<Vec<UserResponse>>()
                .get("/users/{id}", register!(get_user))
                .response::<UserResponse>()
                .as_("getById")
                .post("/users", register!(create_user))
                .json::<CreateUserRequest, UserResponse>()
                .delete("/users/{id}", register!(delete_user))
        })
        .build();

    assert_eq!(routes.len(), 4);
    assert_eq!(routes.routes()[0].name, "listUsers");
    assert_eq!(routes.routes()[1].name, "getById"); // overridden
    assert_eq!(routes.routes()[2].name, "createUser");
    assert_eq!(routes.routes()[3].name, "deleteUser");

    // All grouped
    for r in routes.routes() {
        assert_eq!(r.group.as_deref(), Some("users"));
    }
}

// ---------------------------------------------------------------------------
// Group switching & scoping
// ---------------------------------------------------------------------------

#[test]
fn builder_group_scoping() {
    #[axotyped::endpoint]
async fn health(_auth: TestAuth, State(_s): State<AppState>) {}

    let (_router, routes) = ApiRouter::<AppState>::new()
        .group("users", |g| {
            g.get("/users", register!(list_users)).response::<Vec<UserResponse>>()
        })
        .get("/health", register!(health))
        .as_("health")
        .group("admin", |g| g.delete("/users/{id}", register!(delete_user)))
        .build();

    assert_eq!(routes.routes()[0].group.as_deref(), Some("users"));
    assert_eq!(routes.routes()[1].group, None);
    assert_eq!(routes.routes()[2].group.as_deref(), Some("admin"));
}

// ---------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------

#[test]
fn builder_merge() {
    let users = ApiRouter::<AppState>::new().group("users", |g| {
        g.get("/users", register!(list_users)).response::<Vec<UserResponse>>()
    });

    let admin =
        ApiRouter::<AppState>::new().group("admin", |g| g.delete("/users/{id}", register!(delete_user)));

    let (_router, routes) = ApiRouter::<AppState>::new()
        .merge(users)
        .merge(admin)
        .build();

    assert_eq!(routes.len(), 2);
    assert_eq!(routes.routes()[0].group.as_deref(), Some("users"));
    assert_eq!(routes.routes()[1].group.as_deref(), Some("admin"));
}

// ---------------------------------------------------------------------------
// Query and redirect
// ---------------------------------------------------------------------------

#[test]
fn builder_query_type() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .get("/users", register!(list_users))
        .query::<ListQuery>()
        .response::<Vec<UserResponse>>()
        .build();

    let r = &routes.routes()[0];
    assert!(r.query_type.as_ref().unwrap().contains("ListQuery"));
}

#[test]
fn builder_redirect() {
    #[axotyped::endpoint]
async fn authorize(_auth: TestAuth, State(_s): State<AppState>) {}

    let (_router, routes) = ApiRouter::<AppState>::new()
        .get("/oauth/{provider}/authorize", register!(authorize))
        .redirect()
        .build();

    let r = &routes.routes()[0];
    assert!(r.redirect);
    // deny-by-default: redirect routes are private too (open-redirect hardening)
    assert!(r.is_credentialed());
    assert_eq!(r.path_params[0].name, "provider");
    assert_eq!(r.name, "authorize");
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

#[test]
fn builder_websocket_typed() {
    #[derive(Deserialize)]
    #[derive(axotyped::TS)]
    struct WsParams {
        _token: String,
    }

    #[derive(serde::Serialize)]
    #[derive(axotyped::TS)]
    struct ClientEvent {
        _msg: String,
    }

    #[derive(serde::Serialize)]
    #[derive(axotyped::TS)]
    struct ServerEvent {
        _reply: String,
    }

    #[axotyped::endpoint]
async fn ws_upgrade(_auth: TestAuth, State(_s): State<AppState>) {}

    let (_router, routes) = ApiRouter::<AppState>::new()
        .ws("/ws", register!(ws_upgrade))
        .query::<WsParams>()
        .events::<ClientEvent, ServerEvent>()
        .build();

    let r = &routes.routes()[0];
    assert!(r.websocket);
    assert!(!r.redirect);
    // deny-by-default: WS routes are private by default
    assert!(r.is_credentialed());
    assert!(r.query_type.as_ref().unwrap().contains("WsParams"));
    assert!(r.ws_send_type.as_ref().unwrap().contains("ClientEvent"));
    assert!(r.ws_receive_type.as_ref().unwrap().contains("ServerEvent"));
    assert_eq!(r.name, "wsUpgrade");
}

#[test]
fn builder_websocket_with_path_params() {
    #[derive(serde::Serialize)]
    #[derive(axotyped::TS)]
    struct ClientEvent {
        _msg: String,
    }

    #[derive(serde::Serialize)]
    #[derive(axotyped::TS)]
    struct ServerEvent {
        _reply: String,
    }

    #[axotyped::endpoint]
async fn session_ws(_auth: TestAuth, State(_s): State<AppState>, Path(_id): Path<String>) {}

    let (_router, routes) = ApiRouter::<AppState>::new()
        .ws("/ws/{sessionId}", register!(session_ws))
        .events::<ClientEvent, ServerEvent>()
        .build();

    let r = &routes.routes()[0];
    assert!(r.websocket);
    assert_eq!(r.path_params[0].name, "sessionId");
    assert!(r.ws_send_type.as_ref().unwrap().contains("ClientEvent"));
    assert!(r.ws_receive_type.as_ref().unwrap().contains("ServerEvent"));
    assert_eq!(r.name, "sessionWs");
}

#[test]
fn builder_websocket_with_auth() {
    #[derive(serde::Serialize)]
    #[derive(axotyped::TS)]
    struct ClientEvent {
        _msg: String,
    }

    #[derive(serde::Serialize)]
    #[derive(axotyped::TS)]
    struct ServerEvent {
        _reply: String,
    }

    #[axotyped::endpoint]
    async fn ws_upgrade(_auth: TestAuth, State(_s): State<AppState>) {}

    let (_router, routes) = ApiRouter::<AppState>::new()
        .ws("/ws", register!(ws_upgrade))
        .events::<ClientEvent, ServerEvent>()
        .build();

    let r = &routes.routes()[0];
    assert!(r.websocket);
    assert!(r.is_credentialed());
}

#[test]
fn builder_websocket_custom_name() {
    #[derive(serde::Serialize)]
    #[derive(axotyped::TS)]
    struct ClientEvent {
        _msg: String,
    }

    #[derive(serde::Serialize)]
    #[derive(axotyped::TS)]
    struct ServerEvent {
        _reply: String,
    }

    #[axotyped::endpoint]
async fn ws_handler(_auth: TestAuth, State(_s): State<AppState>) {}

    let (_router, routes) = ApiRouter::<AppState>::new()
        .ws("/ws", register!(ws_handler))
        .events::<ClientEvent, ServerEvent>()
        .as_("connect")
        .build();

    let r = &routes.routes()[0];
    assert_eq!(r.name, "connect");
}

// ---------------------------------------------------------------------------
// End-to-end: generates valid TypeScript
// ---------------------------------------------------------------------------

#[test]
fn builder_generates_valid_ts() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .group("users", |g| {
            g.get("/users", register!(list_users))
                .response::<Vec<UserResponse>>()
                .post("/users", register!(create_user))
                .json::<CreateUserRequest, UserResponse>()
        })
        .build();

    let config = axotyped::GeneratorConfig {
        factory_name: "createApiClient".into(),
        ..Default::default()
    };

    let output = axotyped::generate(&routes, &config);
    assert!(output.contains("listUsers"));
    assert!(output.contains("createUser"));
    assert!(output.contains("UserResponse[]")); // Vec<UserResponse> → UserResponse[]
    assert!(output.contains("\"users\":")); // group (quoted+escaped property key)
}

// ---------------------------------------------------------------------------
// group_prefixed: closure-based grouping with prefix + auth
// ---------------------------------------------------------------------------

#[test]
fn group_does_not_apply_prefix() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .group("auth", |g| {
            g.get("/login", register!(list_users)).response::<Vec<UserResponse>>()
        })
        .build();

    let r = &routes.routes()[0];
    assert_eq!(r.path, "/login");
    assert_eq!(r.group.as_deref(), Some("auth"));
}

#[test]
fn group_prefixed_applies_prefix() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .group_prefixed("admin", |g| {
            g.get("/users", register!(list_users)).response::<Vec<UserResponse>>()
        })
        .build();

    let r = &routes.routes()[0];
    assert_eq!(r.path, "/admin/users");
    assert_eq!(r.group.as_deref(), Some("admin"));
}

#[test]
fn group_prefixed_private_by_default() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .group_prefixed("admin", |g| {
            g.get("/users", register!(list_users))
                .response::<Vec<UserResponse>>()
                .post("/users", register!(create_user))
                .json::<CreateUserRequest, UserResponse>()
        })
        .build();

    assert!(
        routes.routes()[0].is_credentialed(),
        "deny-by-default: GET routes are private without any annotation"
    );
    assert!(
        routes.routes()[1].is_credentialed(),
        "deny-by-default: POST routes are private without any annotation"
    );
}

#[test]
fn group_prefixed_custom_prefix() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .group_prefixed("admin", |g| {
            g.set_prefix("/adm")
                .get("/users", register!(list_users))
                .response::<Vec<UserResponse>>()
        })
        .build();

    assert_eq!(routes.routes()[0].path, "/adm/users");
}

#[test]
fn group_prefixed_does_not_leak_state() {
    #[axotyped::endpoint]
    async fn health(_auth: TestAuth, State(_s): State<AppState>) {}

    let (_router, routes) = ApiRouter::<AppState>::new()
        .group_prefixed("admin", |g| g.delete("/users/{id}", register!(delete_user)))
        // Routes after group_prefixed should NOT have admin group/prefix
        .get("/health", register!(health))
        .build();

    let admin_route = &routes.routes()[0];
    assert_eq!(admin_route.path, "/admin/users/{id}");
    assert_eq!(admin_route.group.as_deref(), Some("admin"));
    assert!(admin_route.is_credentialed());

    let health_route = &routes.routes()[1];
    assert_eq!(health_route.path, "/health");
    assert_eq!(health_route.group, None);
    // deny-by-default: no scope means private, same as inside the group
    assert!(health_route.is_credentialed());
}

// ---------------------------------------------------------------------------
// Deny-by-default & group_public
// ---------------------------------------------------------------------------

#[test]
fn routes_are_private_by_default() {
    #[axotyped::endpoint]
    async fn health(_auth: TestAuth, State(_s): State<AppState>) {}

    let (_router, routes) = ApiRouter::<AppState>::new()
        .get("/users", register!(list_users))
        .response::<Vec<UserResponse>>()
        .get("/health", register!(health))
        .as_("health")
        .ws("/events", register!(health))
        
        .build();

    for r in routes.routes() {
        assert!(r.is_credentialed(), "route '{}' must be private by default", r.name);
    }
}

#[test]
fn group_public_marks_routes_public_and_does_not_leak() {
    #[axotyped::endpoint]
async fn hook(_auth: TestAuth, State(_s): State<AppState>) {}

    let (_router, routes) = ApiRouter::<AppState>::new()
        .group_public("webhooks", |g| {
            g.set_prefix("/webhooks")
                .post("/stripe", register!(hook))
                .post("/github", register!(hook))
        })
        .group("admin", |g| g.delete("/users/{id}", register!(delete_user)))
        .build();

    assert!(
        !routes.routes()[0].is_credentialed(),
        "group_public route must be public"
    );
    assert!(
        !routes.routes()[1].is_credentialed(),
        "group_public route must be public"
    );
    assert_eq!(routes.routes()[0].path, "/webhooks/stripe");
    assert_eq!(routes.routes()[0].group.as_deref(), Some("webhooks"));

    // public scope must not leak into sibling scopes
    assert!(
        routes.routes()[2].is_credentialed(),
        "routes after a group_public are private again"
    );
}

#[test]
fn group_permissive_marks_routes_permissive_and_does_not_leak() {
    #[axotyped::endpoint]
    async fn hook(_auth: TestAuth, State(_s): State<AppState>) {}

    let (_router, routes) = ApiRouter::<AppState>::new()
        .group_permissive("feed", |g| {
            g.get("/feed", register!(hook)).get("/trending", register!(hook))
        })
        .delete("/users/{id}", register!(delete_user))
        .build();

    for r in &routes.routes()[..2] {
        assert_eq!(r.visibility, Visibility::Permissive);
        assert_eq!(r.declared, Visibility::Permissive);
        assert!(r.is_credentialed(), "permissive stays credentialed");
    }
    assert_eq!(
        routes.routes()[2].visibility,
        Visibility::Private,
        "routes after a group_permissive are private again"
    );
}

#[test]
fn group_permissive_forwards_from_mid_chain_builders() {
    #[axotyped::endpoint]
    async fn hook(_auth: TestAuth, State(_s): State<AppState>) {}

    // RouteBuilder forward: chain starting with an HTTP route.
    let (_router, routes) = ApiRouter::<AppState>::new()
        .get("/health", register!(hook))
        .group_permissive("feed", |g| g.get("/feed", register!(hook)))
        .build();
    assert_eq!(routes.routes()[1].visibility, Visibility::Permissive);

    // WsRouteBuilder forward: chain starting with a WebSocket route.
    let (_router, routes) = ApiRouter::<AppState>::new()
        .ws("/events", register!(hook))
        .group_permissive("feed", |g| g.get("/feed", register!(hook)))
        .build();
    assert_eq!(routes.routes()[1].visibility, Visibility::Permissive);
}

#[test]
fn group_public_forwards_from_mid_chain_builders() {
    #[axotyped::endpoint]
    async fn hook(_auth: TestAuth, State(_s): State<AppState>) {}

    let (_router, routes) = ApiRouter::<AppState>::new()
        .get("/health", register!(hook))
        .group_public("webhooks", |g| g.post("/stripe", register!(hook)))
        .build();
    assert_eq!(routes.routes()[1].visibility, Visibility::Public);
}

#[test]
fn nested_group_inside_group_public_inherits_publicity() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .group_public("pub", |g| {
            g.group_prefixed("deep", |h| h.get("/res", register!(list_users)))
        })
        .build();

    let r = &routes.routes()[0];
    assert_eq!(r.path, "/deep/res");
    // group names overwrite (not nest), consistent with `group`/`group_prefixed`
    assert_eq!(r.group.as_deref(), Some("deep"));
    assert!(!r.is_credentialed(), "nested groups inherit the public scope");
}

#[test]
fn group_prefixed_multiple_methods() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .group_prefixed("admin", |g| {
            g.get("/users", register!(list_users))
                .response::<Vec<UserResponse>>()
                .post("/users", register!(create_user))
                .json::<CreateUserRequest, UserResponse>()
                .delete("/users/{id}", register!(delete_user))
        })
        .build();

    assert_eq!(routes.len(), 3);
    for r in routes.routes() {
        assert!(r.is_credentialed(), "all routes should have auth");
        assert!(
            r.path.starts_with("/admin/"),
            "path should have prefix: got {}",
            r.path
        );
        assert_eq!(r.group.as_deref(), Some("admin"));
    }
}

// ---------------------------------------------------------------------------
// RouteTable trait (manual struct implementation)
// ---------------------------------------------------------------------------

#[test]
fn route_table_trait_manual_impl() {
    use axotyped::{Collector, RouteTable};

    struct ManualRoutes;

    impl RouteTable<AppState> for ManualRoutes {
        fn define<C: Collector>(r: ApiRouter<AppState, C>) -> ApiRouter<AppState, C> {
            r.get("/health", register!(list_users))
                .as_("health")
                .group_prefixed("admin", |g| g.delete("/users/{id}", register!(delete_user)))
        }
    }

    let _router = ManualRoutes::router();
    let collection = ManualRoutes::collect_types();

    assert_eq!(collection.len(), 2);
    assert_eq!(collection.routes()[0].name, "health");
    assert_eq!(collection.routes()[1].path, "/admin/users/{id}");
}

#[test]
fn set_prefix_without_group_with() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .set_prefix("/api/v1")
        .get("/users", register!(list_users))
        .response::<Vec<UserResponse>>()
        .build();

    let r = &routes.routes()[0];
    assert_eq!(r.path, "/api/v1/users");
    assert!(r.is_credentialed());
}

#[test]
fn define_routes_custom_identifier_name() {
    use axotyped::define_routes;

    define_routes! {
        pub CustomRoutes for AppState, |route| {
            route.get("/health", register!(list_users)).as_("health")
        }
    }

    let _router = CustomRoutes::router();
    let collection = CustomRoutes::collect_types();

    assert_eq!(collection.len(), 1);
    assert_eq!(collection.routes()[0].name, "health");
}

// ---------------------------------------------------------------------------
// Server-derived auth metadata (auth_layer)
// ---------------------------------------------------------------------------

/// Pass-through middleware standing in for real authentication.
async fn auth_mw(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    next.run(req).await
}

#[test]
fn auth_layer_derives_auth_metadata_inside_public_scope() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .group_public("pub", |g| {
            g.get("/open", register!(list_users))
                
                .auth_layer(axum::middleware::from_fn(auth_mw))
                .get("/gated", register!(get_user))
        })
        .build();

    let open = routes.routes().iter().find(|r| r.path == "/open").unwrap();
    let gated = routes.routes().iter().find(|r| r.path == "/gated").unwrap();
    assert!(!open.is_credentialed(), "route before the auth layer stays public");
    assert!(
        open.declared == Visibility::Public,
        "public-scope membership is itself the declaration"
    );
    assert!(
        gated.is_credentialed(),
        "auth layer derives authenticated metadata for subsequent routes"
    );
    assert!(
        gated.declared == Visibility::Public,
        "the scope's public declaration is preserved so generation can flag the contradiction"
    );
}

#[test]
fn auth_layer_overrides_public_declarations_and_records_contradiction() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .group_public("mixed", |g| {
            g.auth_layer(axum::middleware::from_fn(auth_mw))
                .post("/hook", register!(create_user))
        })
        .build();

    let r = &routes.routes()[0];
    assert!(
        r.is_credentialed(),
        "server enforcement wins: protected route is authenticated"
    );
    assert!(
        r.declared == Visibility::Public,
        "the public declaration is preserved so generation can flag it"
    );
}

#[test]
fn auth_layer_does_not_leak_to_sibling_scopes() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .group_public("pub", |g| {
            g.group_prefixed("admin", |a| {
                a.auth_layer(axum::middleware::from_fn(auth_mw))
                    .get("/inner", register!(get_user))
            })
            .group_prefixed("other", |o| o.get("/sibling", register!(get_user)))
        })
        .build();

    let inner = routes
        .routes()
        .iter()
        .find(|r| r.path == "/admin/inner")
        .unwrap();
    let sibling = routes
        .routes()
        .iter()
        .find(|r| r.path == "/other/sibling")
        .unwrap();
    assert!(inner.is_credentialed());
    assert!(!sibling.is_credentialed(), "protection must not leak to sibling scopes");
}

#[test]
fn auth_layer_inherits_into_nested_groups() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .group_prefixed("v1", |v1| {
            v1.auth_layer(axum::middleware::from_fn(auth_mw))
                .group_prefixed("admin", |a| a.get("/users", register!(list_users)))
        })
        .build();

    let r = &routes.routes()[0];
    assert_eq!(r.path, "/v1/admin/users");
    assert!(r.is_credentialed(), "nested groups inherit the protected scope");
}

#[test]
fn plain_layer_leaves_auth_metadata_untouched() {
    let (_router, routes) = ApiRouter::<AppState>::new()
        .group_public("pub", |g| {
            g.layer(axum::middleware::from_fn(auth_mw))
                .get("/rate_limited", register!(get_user))
        })
        .build();

    let r = &routes.routes()[0];
    assert!(
        !r.is_credentialed(),
        ".layer() is a transport concern, not authentication"
    );
    assert!(
        r.declared == Visibility::Public,
        "public-scope membership is preserved regardless of plain layering"
    );
}
