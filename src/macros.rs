/// Declarative macro for defining API route metadata.
///
/// # Syntax
///
/// ```rust,ignore
/// use axotyped::api_routes;
///
/// let routes = api_routes! {
///     @group emailPassword
///
///     register: POST "/register"
///         body: RegisterRequest -> MessageResponse;
///     login: POST "/login"
///         body: LoginRequest -> LoginResponse;
///     verify: POST "/verify-email"
///         body: VerifyEmailRequest -> MessageResponse;
///     changePassword: POST "/change-password"
///         body: ChangePasswordRequest -> MessageResponse;
///     listUsers: GET "/admin/users"
///         query: ListUsersQuery -> Vec<UserResponse>;
///     getUser: GET "/admin/users/{id}"
///         -> UserResponse;
///     healthCheck: GET "/health" [public]
///     authorize: GET "/oauth/{provider}/authorize" [redirect]
///         query: AuthorizeQuery;
/// };
/// ```
///
/// # Elements
///
/// Routes require authentication by default; `[public]` opts out.
///
/// - `@group <name>` — sets the group for all following routes (generates nested object)
/// - `[public]` — marks route as public (no auth in the generated client)
/// - `[redirect]` — marks route as a browser redirect (URL builder, not fetch)
/// - `[ws]` — marks route as a WebSocket endpoint (generates typed WS client)
/// - `send: <Type>` — client-to-server event type for WS routes
/// - `receive: <Type>` — server-to-client event type for WS routes
/// - `body: <Type>` — request body type (supports `Vec<T>` and `Option<T>`)
/// - `query: <Type>` — query parameters type (supports `Vec<T>` and `Option<T>`)
/// - `-> <Type>` — response type (supports `Vec<T>` and `Option<T>`, omit for void)
/// - `{param}` in paths — path parameters (become function args)
#[macro_export]
macro_rules! api_routes {
    // Entry point — parse all statements
    (@collect $collection:ident, @group_ctx $group:expr, ) => {};

    // @group directive — set group context for subsequent routes
    (@collect $collection:ident, @group_ctx $_old_group:expr,
        @group $new_group:ident
        $($rest:tt)*
    ) => {
        $crate::api_routes!(@collect $collection, @group_ctx Some(stringify!($new_group).to_string()), $($rest)*);
    };

    // @nogroup directive — clear group context
    (@collect $collection:ident, @group_ctx $_old_group:expr,
        @nogroup
        $($rest:tt)*
    ) => {
        $crate::api_routes!(@collect $collection, @group_ctx Option::<String>::None, $($rest)*);
    };

    // ---------------------------------------------------------------------------
    // Route matchers — one arm for each combination of generic/plain types.
    // We need separate arms because macro_rules can't use `ty` followed by
    // keywords like `query` or `->`.
    // ---------------------------------------------------------------------------

    // WS route with send:/receive: event types.
    // Syntax: send: SendType, receive: ReceiveType (comma-separated, required)
    (@collect $collection:ident, @group_ctx $group:expr,
        $name:ident : $method:ident $path:literal
        $([$($flag:ident),*])?
        send: $so:ident $(<$si:ident>)?, receive: $ro2:ident $(<$ri2:ident>)?
        $(query: $qo:ident $(<$qi:ident>)?)?
        ;
        $($rest:tt)*
    ) => {
        $collection.push($crate::RouteDefinition {
            name: stringify!($name).to_string(),
            method: $crate::api_routes!(@method $method),
            path: $path.to_string(),
            visibility: $crate::api_routes!(@visibility $([$($flag),*])?),
            declared: $crate::api_routes!(@visibility $([$($flag),*])?),
            body_type: None,
            response_type: None,
            query_type: $crate::api_routes!(@opt_type $($qo $(<$qi>)?)?),
            path_params: $crate::extract_path_params($path),
            group: $group.clone(),
            allow_redirects: $crate::api_routes!(@has_flag allow_redirects $([$($flag),*])?),
            redirect: false,
            websocket: true,
            ws_send_type: $crate::api_routes!(@opt_type $so $(<$si>)?),
            ws_receive_type: $crate::api_routes!(@opt_type $ro2 $(<$ri2>)?),
        });
        $crate::api_routes!(@collect $collection, @group_ctx $group, $($rest)*);
    };

    // General route matcher — body, query, response types
    (@collect $collection:ident, @group_ctx $group:expr,
        $name:ident : $method:ident $path:literal
        $([$($flag:ident),*])?
        $(body: $bo:ident $(<$bi:ident>)?)?
        $(query: $qo:ident $(<$qi:ident>)?)?
        $(-> $ro:ident $(<$ri:ident>)?)?
        ;
        $($rest:tt)*
    ) => {
        $collection.push($crate::RouteDefinition {
            name: stringify!($name).to_string(),
            method: $crate::api_routes!(@method $method),
            path: $path.to_string(),
            visibility: $crate::api_routes!(@visibility $([$($flag),*])?),
            declared: $crate::api_routes!(@visibility $([$($flag),*])?),
            body_type: $crate::api_routes!(@opt_type $($bo $(<$bi>)?)?),
            response_type: $crate::api_routes!(@opt_type $($ro $(<$ri>)?)?),
            query_type: $crate::api_routes!(@opt_type $($qo $(<$qi>)?)?),
            path_params: $crate::extract_path_params($path),
            group: $group.clone(),
            allow_redirects: $crate::api_routes!(@has_flag allow_redirects $([$($flag),*])?),
            redirect: $crate::api_routes!(@has_flag redirect $([$($flag),*])?),
            websocket: $crate::api_routes!(@has_ws_flag $([$($flag),*])?),
            ws_send_type: None,
            ws_receive_type: None,
        });
        $crate::api_routes!(@collect $collection, @group_ctx $group, $($rest)*);
    };

    // Method helpers
    (@method GET) => { $crate::HttpMethod::Get };
    (@method POST) => { $crate::HttpMethod::Post };
    (@method PUT) => { $crate::HttpMethod::Put };
    (@method PATCH) => { $crate::HttpMethod::Patch };
    (@method DELETE) => { $crate::HttpMethod::Delete };

    // Flag detection helpers
    (@has_flag $target:ident) => { false };
    (@has_flag $target:ident [$($flag:ident),*]) => {
        $crate::api_routes!(@check_flag $target, $($flag),*)
    };
    // Visibility from flags: public dominates permissive; neither is private.
    // Used for both `visibility` (effective) and `declared` — the macro
    // context has no layers, so nothing overrides either.
    (@visibility) => { $crate::Visibility::Private };
    (@visibility [$($flag:ident),*]) => {
        if $crate::api_routes!(@has_flag public [$($flag),*]) {
            $crate::Visibility::Public
        } else if $crate::api_routes!(@has_flag permissive [$($flag),*]) {
            $crate::Visibility::Permissive
        } else {
            $crate::Visibility::Private
        }
    };
    (@check_flag $target:ident, ) => { false };
    (@check_flag $target:ident, $target2:ident $(, $rest:ident)*) => {
        $crate::api_routes!(@flag_eq $target $target2) || $crate::api_routes!(@check_flag $target, $($rest),*)
    };

    // Compare two idents for equality via stringify
    (@flag_eq $a:ident $b:ident) => {
        {
            // This is a const-evaluable string comparison
            const A: &str = stringify!($a);
            const B: &str = stringify!($b);
            A.len() == B.len() && {
                let a = A.as_bytes();
                let b = B.as_bytes();
                let mut i = 0;
                let mut eq = true;
                while i < a.len() {
                    if a[i] != b[i] {
                        eq = false;
                    }
                    i += 1;
                }
                eq
            }
        }
    };

    // WebSocket flag helper
    (@has_ws_flag) => { false };
    (@has_ws_flag [$($flag:ident),*]) => {
        $crate::api_routes!(@check_ws_flag $($flag),*)
    };
    (@check_ws_flag) => { false };
    (@check_ws_flag ws $(, $rest:ident)*) => { true };
    (@check_ws_flag $other:ident $(, $rest:ident)*) => {
        $crate::api_routes!(@check_ws_flag $($rest),*)
    };

    // Optional type helpers — handles plain idents and Generic<Inner>
    (@opt_type) => { None };
    (@opt_type $outer:ident < $inner:ident >) => {
        Some({
            let mut s = String::from(stringify!($outer));
            s.push('<');
            s.push_str(stringify!($inner));
            s.push('>');
            s
        })
    };
    (@opt_type $ty:ident) => { Some(stringify!($ty).to_string()) };

    // Top-level entry point
    {
        $($tokens:tt)*
    } => {
        {
            let mut collection = $crate::RouteCollection::new();
            $crate::api_routes!(@collect collection, @group_ctx Option::<String>::None, $($tokens)*);
            collection
        }
    };
}
