#[macro_use]
mod common;

use axotyped::HttpMethod;

#[test]
fn simple_get_route() {
    let routes = api_routes! {
        getSession: GET "/session"
            -> SessionResponse;
    };
    assert_eq!(routes.len(), 1);
    let r = &routes.routes()[0];
    assert_eq!(r.name, "getSession");
    assert_eq!(r.method, HttpMethod::Get);
    assert_eq!(r.path, "/session");
    assert!(r.is_credentialed());
    assert_eq!(r.response_type.as_deref(), Some("SessionResponse"));
    assert!(r.body_type.is_none());
    assert!(r.query_type.is_none());
    assert!(r.group.is_none());
    assert!(!r.redirect);
    assert!(!r.websocket);
}

#[test]
fn post_with_body_and_response() {
    let routes = api_routes! {
        register: POST "/register"
            body: RegisterRequest -> MessageResponse;
    };
    let r = &routes.routes()[0];
    assert_eq!(r.name, "register");
    assert_eq!(r.method, HttpMethod::Post);
    assert!(r.is_credentialed(), "deny-by-default");
    assert_eq!(r.body_type.as_deref(), Some("RegisterRequest"));
    assert_eq!(r.response_type.as_deref(), Some("MessageResponse"));
}

#[test]
fn route_with_group() {
    let routes = api_routes! {
        @group emailPassword

        register: POST "/register"
            body: RegisterRequest -> MessageResponse;
        login: POST "/login"
            body: LoginRequest -> LoginResponse;
    };
    assert_eq!(routes.len(), 2);
    assert_eq!(routes.routes()[0].group.as_deref(), Some("emailPassword"));
    assert_eq!(routes.routes()[1].group.as_deref(), Some("emailPassword"));
}

#[test]
fn route_with_path_params() {
    let routes = api_routes! {
        getUser: GET "/admin/users/{id}"
            -> UserResponse;
    };
    let r = &routes.routes()[0];
    assert_eq!(r.path_params.len(), 1);
    assert_eq!(r.path_params[0].name, "id");
}

#[test]
fn route_with_query_params() {
    let routes = api_routes! {
        listUsers: GET "/admin/users"
            query: ListUsersQuery -> ListUsersResponse;
    };
    let r = &routes.routes()[0];
    assert_eq!(r.query_type.as_deref(), Some("ListUsersQuery"));
    assert_eq!(r.response_type.as_deref(), Some("ListUsersResponse"));
    assert!(r.is_credentialed());
}

#[test]
fn redirect_route() {
    let routes = api_routes! {
        authorize: GET "/oauth/{provider}/authorize" [redirect]
            query: AuthorizeQuery;
    };
    let r = &routes.routes()[0];
    assert!(r.redirect);
    assert!(r.is_credentialed(), "deny-by-default");
    assert_eq!(r.path_params.len(), 1);
    assert_eq!(r.path_params[0].name, "provider");
    assert_eq!(r.query_type.as_deref(), Some("AuthorizeQuery"));
    assert!(r.response_type.is_none());
}

#[test]
fn websocket_route() {
    let routes = api_routes! {
        wsUpgrade: GET "/ws" [ws]
            send: ClientEvent, receive: ServerEvent
            query: WsParams;
    };
    let r = &routes.routes()[0];
    assert!(r.websocket);
    assert!(!r.redirect);
    assert!(r.is_credentialed(), "deny-by-default");
    assert_eq!(r.query_type.as_deref(), Some("WsParams"));
    assert_eq!(r.ws_send_type.as_deref(), Some("ClientEvent"));
    assert_eq!(r.ws_receive_type.as_deref(), Some("ServerEvent"));
    assert!(r.response_type.is_none());
}

#[test]
fn websocket_with_auth() {
    let routes = api_routes! {
        wsConnect: GET "/ws/{sessionId}" [ws]
            send: ClientEvent, receive: ServerEvent
            query: WsParams;
    };
    let r = &routes.routes()[0];
    assert!(r.websocket);
    assert!(r.is_credentialed());
    assert_eq!(r.path_params[0].name, "sessionId");
    assert_eq!(r.ws_send_type.as_deref(), Some("ClientEvent"));
    assert_eq!(r.ws_receive_type.as_deref(), Some("ServerEvent"));
}

#[test]
fn multiple_flags() {
    let routes = api_routes! {
        protectedRedirect: GET "/oauth/{provider}/link" [redirect]
            -> LinkResponse;
    };
    let r = &routes.routes()[0];
    assert!(r.is_credentialed());
    assert!(r.redirect);
}

#[test]
fn nogroup_clears_context() {
    let routes = api_routes! {
        @group myGroup

        a: GET "/a" -> AResponse;

        @nogroup

        b: GET "/b" -> BResponse;
    };
    assert_eq!(routes.routes()[0].group.as_deref(), Some("myGroup"));
    assert_eq!(routes.routes()[1].group, None);
}

#[test]
fn multiple_groups() {
    let routes = api_routes! {
        @group emailPassword

        register: POST "/register"
            body: RegisterRequest -> MessageResponse;

        @group passkey

        loginBegin: POST "/passkey/login/begin"
            body: PasskeyLoginBeginRequest -> PasskeyLoginBeginResponse;
    };
    assert_eq!(routes.len(), 2);
    assert_eq!(routes.routes()[0].group.as_deref(), Some("emailPassword"));
    assert_eq!(routes.routes()[1].group.as_deref(), Some("passkey"));
}

#[test]
fn delete_method() {
    let routes = api_routes! {
        deletePasskey: DELETE "/passkeys/{id}"
            -> MessageResponse;
    };
    let r = &routes.routes()[0];
    assert_eq!(r.method, HttpMethod::Delete);
}

#[test]
fn put_method() {
    let routes = api_routes! {
        updateUser: PUT "/admin/users/{id}"
            body: UpdateUserRequest -> UserResponse;
    };
    let r = &routes.routes()[0];
    assert_eq!(r.method, HttpMethod::Put);
}

#[test]
fn patch_method() {
    let routes = api_routes! {
        updateProfile: PATCH "/me"
            body: UpdateProfileRequest -> ProfileResponse;
    };
    let r = &routes.routes()[0];
    assert_eq!(r.method, HttpMethod::Patch);
}

#[test]
fn no_response_type() {
    let routes = api_routes! {
        deleteItem: DELETE "/items/{id}";
    };
    let r = &routes.routes()[0];
    assert!(r.response_type.is_none());
    assert!(r.body_type.is_none());
}

#[test]
fn body_only_no_response() {
    let routes = api_routes! {
        doSomething: POST "/action"
            body: ActionRequest;
    };
    let r = &routes.routes()[0];
    assert_eq!(r.body_type.as_deref(), Some("ActionRequest"));
    assert!(r.response_type.is_none());
}

#[test]
fn vec_response_type() {
    let routes = api_routes! {
        listUsers: GET "/admin/users"
            -> Vec<UserResponse>;
    };
    let r = &routes.routes()[0];
    assert_eq!(r.response_type.as_deref(), Some("Vec<UserResponse>"));
}

#[test]
fn option_response_type() {
    let routes = api_routes! {
        getUser: GET "/users/{id}"
            -> Option<UserResponse>;
    };
    let r = &routes.routes()[0];
    assert_eq!(r.response_type.as_deref(), Some("Option<UserResponse>"));
}

#[test]
fn vec_body_type() {
    let routes = api_routes! {
        batchCreate: POST "/items"
            body: Vec<CreateItemRequest> -> Vec<ItemResponse>;
    };
    let r = &routes.routes()[0];
    assert_eq!(r.body_type.as_deref(), Some("Vec<CreateItemRequest>"));
    assert_eq!(r.response_type.as_deref(), Some("Vec<ItemResponse>"));
}

#[test]
fn vec_query_type() {
    let routes = api_routes! {
        listRuns: GET "/api/runs"
            query: RunListQuery -> Vec<RunResponse>;
    };
    let r = &routes.routes()[0];
    assert_eq!(r.query_type.as_deref(), Some("RunListQuery"));
    assert_eq!(r.response_type.as_deref(), Some("Vec<RunResponse>"));
}

#[test]
fn collection_extend() {
    let mut core = api_routes! {
        getSession: GET "/session"
            -> SessionResponse;
    };

    let email_password = api_routes! {
        @group emailPassword
        register: POST "/register"
            body: RegisterRequest -> MessageResponse;
    };

    core.extend(email_password);
    assert_eq!(core.len(), 2);
    assert!(core.routes()[0].group.is_none());
    assert_eq!(core.routes()[1].group.as_deref(), Some("emailPassword"));
}

// ===========================================================================
// Deny-by-default & public declarations
// ===========================================================================

#[test]
fn public_flag_opts_out_of_auth() {
    let routes = api_routes! {
        healthCheck: GET "/health" [public];
        deleteItem: DELETE "/items/{id}";
    };
    assert!(!routes.routes()[0].is_credentialed(), "[public] must clear auth");
    assert!(routes.routes()[1].is_credentialed(), "unflagged routes stay private");
}

mod endpoint_visibility {
    use axotyped::{ApiRouter, Visibility, endpoint};

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

    #[endpoint]
    pub async fn admin_thing(_auth: TestAuth) -> &'static str {
        "secret"
    }

    #[endpoint(public)]
    pub async fn public_health() -> &'static str {
        "ok"
    }

    #[endpoint(permissive)]
    pub async fn feed() -> &'static str {
        "items"
    }

    fn build(routes_def: impl FnOnce(ApiRouter<()>) -> ApiRouter<()>) -> Vec<Visibility> {
        let (_router, routes) = routes_def(ApiRouter::<()>::new()).build();
        routes.routes().iter().map(|r| r.visibility).collect()
    }

    #[test]
    fn endpoint_without_public_is_private_by_default() {
        let flags = build(|r| {
            r.get("/thing", axotyped::register!(admin_thing))
                .as_("thing")
        });
        assert_eq!(
            flags,
            vec![Visibility::Private],
            "#[endpoint] without `public` stays private"
        );
    }

    #[test]
    fn endpoint_public_makes_route_public() {
        let flags = build(|r| {
            r.get("/health", axotyped::register!(public_health))
                .as_("health")
        });
        assert_eq!(
            flags,
            vec![Visibility::Public],
            "#[endpoint(public)] must open the route"
        );
    }

    #[test]
    fn endpoint_permissive_marks_route_permissive_but_credentialed() {
        let (_router, routes) = ApiRouter::<()>::new()
            .get("/feed", axotyped::register!(feed))
            .as_("feed")
            
            .build();
        let r = &routes.routes()[0];
        assert_eq!(r.visibility, Visibility::Permissive);
        assert_eq!(r.declared, Visibility::Permissive);
        assert!(r.is_credentialed(), "permissive stays credentialed for guard/cache");
    }
}

// -- bare-name shadowing of axum built-ins -----------------------------------
//
// `is_ambient_extractor` assumes a bare built-in name is axum's. On a
// credentialless declaration that assumption is type-checked: an
// ambient-classified parameter must truly be ambient. A shadowing
// extractor that genuinely admits anonymous callers passes; the
// `#[credential]` shadow fails (compile_fail doctest on the params
// module) with the same contradiction error as any custom credential
// parameter.

#[axotyped::optional_credential]
pub struct Path;

#[axotyped::endpoint(public)]
async fn shadowed_optional_path(_p: Path) {}

#[test]
fn shadowing_built_in_name_with_optional_credential_compiles_on_public() {
    // Compilation of `shadowed_optional_path` above IS the assertion: its
    // `__param_assertions` ran `__truly_ambient::<Path>()`, which holds
    // because `Auth = OptionalCredential`.
    let _ = shadowed_optional_path;
}

// -- common axum extractors carry EndpointParam impls ------------------------
//
// #[endpoint] asserts EndpointParam for EVERY parameter and the orphan rule
// bars downstream crates from adding impls for foreign types — compilation
// of these handlers IS the assertion that the extractor set is usable at all.

#[derive(serde::Deserialize, Default)]
pub struct FormData {
    pub name: String,
}

#[axotyped::endpoint]
async fn submit_form(axum::Form(_form): axum::Form<FormData>) {}

// (No `Result<T, E>` fixture: axum 0.8 has no Result extractor — the shape
// cannot reach a handler signature, so it carries no EndpointParam impl.)

#[axotyped::endpoint]
async fn echo_raw(_body: String) {}

#[test]
fn common_axum_extractors_are_endpoint_params() {
    let _ = submit_form;
    let _ = echo_raw;
}
