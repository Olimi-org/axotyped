//! Fixture shorthand shared by the integration tests, crate-internal by
//! origin and now physically so: the string-table grammar lives where its
//! only users (these tests) live. It builds RouteCollections without
//! handlers — real route definitions go through `register!` +
//! `#[endpoint]` or the `RouteTable` trait.

macro_rules! api_routes {
    // Entry point — parse all statements
    (@collect $collection:ident, @group_ctx $group:expr, ) => {};

    // @group directive — set group context for subsequent routes
    (@collect $collection:ident, @group_ctx $_old_group:expr,
        @group $new_group:ident
        $($rest:tt)*
    ) => {
        api_routes!(@collect $collection, @group_ctx Some(stringify!($new_group).to_string()), $($rest)*);
    };

    // @nogroup directive — clear group context
    (@collect $collection:ident, @group_ctx $_old_group:expr,
        @nogroup
        $($rest:tt)*
    ) => {
        api_routes!(@collect $collection, @group_ctx Option::<String>::None, $($rest)*);
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
        $collection.push(::axotyped::RouteDefinition {
            name: stringify!($name).to_string(),
            method: api_routes!(@method $method),
            path: $path.to_string(),
            visibility: api_routes!(@visibility $([$($flag),*])?),
            declared: api_routes!(@visibility $([$($flag),*])?),
            body_type: None,
            response_type: None,
            query_type: api_routes!(@opt_type $($qo $(<$qi>)?)?),
            path_params: ::axotyped::extract_path_params($path),
            group: $group.clone(),
            allow_redirects: api_routes!(@has_flag allow_redirects $([$($flag),*])?),
            redirect: false,
            websocket: true,
            ws_send_type: api_routes!(@opt_type $so $(<$si>)?),
            ws_receive_type: api_routes!(@opt_type $ro2 $(<$ri2>)?),
        });
        api_routes!(@collect $collection, @group_ctx $group, $($rest)*);
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
        $collection.push(::axotyped::RouteDefinition {
            name: stringify!($name).to_string(),
            method: api_routes!(@method $method),
            path: $path.to_string(),
            visibility: api_routes!(@visibility $([$($flag),*])?),
            declared: api_routes!(@visibility $([$($flag),*])?),
            body_type: api_routes!(@opt_type $($bo $(<$bi>)?)?),
            response_type: api_routes!(@opt_type $($ro $(<$ri>)?)?),
            query_type: api_routes!(@opt_type $($qo $(<$qi>)?)?),
            path_params: ::axotyped::extract_path_params($path),
            group: $group.clone(),
            allow_redirects: api_routes!(@has_flag allow_redirects $([$($flag),*])?),
            redirect: api_routes!(@has_flag redirect $([$($flag),*])?),
            websocket: api_routes!(@has_ws_flag $([$($flag),*])?),
            ws_send_type: None,
            ws_receive_type: None,
        });
        api_routes!(@collect $collection, @group_ctx $group, $($rest)*);
    };

    // Method helpers
    (@method GET) => { ::axotyped::HttpMethod::Get };
    (@method POST) => { ::axotyped::HttpMethod::Post };
    (@method PUT) => { ::axotyped::HttpMethod::Put };
    (@method PATCH) => { ::axotyped::HttpMethod::Patch };
    (@method DELETE) => { ::axotyped::HttpMethod::Delete };

    // Flag detection helpers
    (@has_flag $target:ident) => { false };
    (@has_flag $target:ident [$($flag:ident),*]) => {
        api_routes!(@check_flag $target, $($flag),*)
    };
    // Visibility from flags: public dominates permissive; neither is private.
    // Used for both `visibility` (effective) and `declared` — the macro
    // context has no layers, so nothing overrides either.
    (@visibility) => { ::axotyped::Visibility::Private };
    (@visibility [$($flag:ident),*]) => {
        if api_routes!(@has_flag public [$($flag),*]) {
            ::axotyped::Visibility::Public
        } else if api_routes!(@has_flag permissive [$($flag),*]) {
            ::axotyped::Visibility::Permissive
        } else {
            ::axotyped::Visibility::Private
        }
    };
    (@check_flag $target:ident, ) => { false };
    (@check_flag $target:ident, $target2:ident $(, $rest:ident)*) => {
        api_routes!(@flag_eq $target $target2) || api_routes!(@check_flag $target, $($rest),*)
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
        api_routes!(@check_ws_flag $($flag),*)
    };
    (@check_ws_flag) => { false };
    (@check_ws_flag ws $(, $rest:ident)*) => { true };
    (@check_ws_flag $other:ident $(, $rest:ident)*) => {
        api_routes!(@check_ws_flag $($rest),*)
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
            let mut collection = ::axotyped::RouteCollection::new();
            api_routes!(@collect collection, @group_ctx Option::<String>::None, $($tokens)*);
            collection
        }
    };
}
