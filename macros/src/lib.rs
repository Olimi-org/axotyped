//! Proc-macros for axotyped route type inference.
//!
//! - `#[endpoint]` — attribute on handler functions; extracts `Json<T>`, `Query<T>` from
//!   the signature and generates a companion `__EndpointMeta` struct
//! - `register!()` — call-site macro that wraps a handler path with its metadata
//! - `#[derive(TS)]` — generate TypeScript bindings from Rust types

mod ts;

// TS derive macro — must reside in the crate root.
#[proc_macro_derive(TS, attributes(ts))]
pub fn ts_derive(input: TokenStream) -> TokenStream {
    ts::typescript(input)
}

use proc_macro::TokenStream;
use quote::quote;
use syn::{FnArg, ItemFn, PathArguments, ReturnType, Type, parse_macro_input};

// ---------------------------------------------------------------------------
// #[endpoint] — attribute macro on handler functions
// ---------------------------------------------------------------------------

/// Extract request/response types from the handler function signature.
///
/// Generates a companion struct `<fn_name>__EndpointMeta` implementing `EndpointMeta`,
/// which carries the inferred `body_type`, `response_type`, and `query_type`.
///
/// # Visibility
///
/// Routes require authentication unless declared public. Use
/// `#[endpoint(public)]` to mark an individual handler's route public, or
/// `#[endpoint(permissive)]` for routes whose handler runs with or without
/// credentials (the scope-wide alternatives are [`ApiRouter::group_public`]
/// / `group_permissive` on the builder).
///
///
/// # Extracted types
///
/// - **Body type**: inner `T` from `Json<T>` in function parameters
/// - **Query type**: inner `T` from `Query<T>` in function parameters
/// - **Response type**: inner `T` from `Json<T>` in return type (unwraps `Result<Json<T>, _>`)
///
/// Handlers returning `Result<StatusCode, StatusCode>` (no body) produce no response type.
#[proc_macro_attribute]
pub fn endpoint(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_fn = parse_macro_input!(item as ItemFn);

    // Options: `#[endpoint]`, a visibility (`public` | `permissive`), and/or
    // the `allow_redirects` route property — e.g. `#[endpoint(public)]` or
    // `#[endpoint(public, allow_redirects)]`. Anything else is a compile
    // error so typos can never silently flip visibility.
    let mut is_public = false;
    let mut is_permissive = false;
    let mut allow_redirects = false;
    let mut expect_ident = true;
    let mut any_token = false;
    for tt in attr {
        any_token = true;
        match tt {
            proc_macro::TokenTree::Ident(id) if expect_ident => {
                match id.to_string().as_str() {
                    "public" if !is_public && !is_permissive => is_public = true,
                    "permissive" if !is_public && !is_permissive => is_permissive = true,
                    "allow_redirects" if !allow_redirects => allow_redirects = true,
                    _ => return endpoint_attr_error(),
                }
                expect_ident = false;
            }
            proc_macro::TokenTree::Punct(p)
                if p.as_char() == ',' && !expect_ident =>
            {
                expect_ident = true
            }
            _ => return endpoint_attr_error(),
        }
    }
    if expect_ident && any_token {
        return endpoint_attr_error();
    }

    let fn_name = &item_fn.sig.ident;
    let meta_struct_name = quote::format_ident!("{}__EndpointMeta", fn_name);

    let body_type = extract_body_type(&item_fn);
    let query_type = extract_query_type(&item_fn);
    let response_type = extract_response_type(&item_fn);
    let param_types = extract_param_types(&item_fn);

    // Credential classification. Every parameter must implement
    // `EndpointParam` (the assert fn gives the actionable error for a
    // missing impl); the constants then decide, at the type level, whether
    // this handler may register on a private-by-default chain.
    let custom_params: Vec<&Type> = param_types
        .iter()
        .filter(|t| {
            let name = last_segment_ident(t);
            !KNOWN_AMBIENT_EXTRACTORS.contains(&name.as_str())
        })
        .collect();
    let assert_fn_name = quote::format_ident!("{}__param_assertions", fn_name);
    let param_assertions = quote! {
        #[doc(hidden)]
        #[allow(dead_code, non_snake_case)]
        fn #assert_fn_name() {
            fn __param<T: ::axotyped::EndpointParam>() {}
            #(__param::<#param_types>();)*
        }
    };

    // Type-level authorization fold. Starts `Enforced` when the endpoint
    // declared itself public/permissive (its declaration IS the compile-time
    // authorization), then ORs every parameter's credential flag. Ends up on
    // `EndpointMeta::Authorization`; the private-by-default chain bounds
    // registration on it projecting to `Allowed`.
    let mut authz = if is_public || is_permissive {
        quote! { ::axotyped::Credential }
    } else {
        quote! { ::axotyped::OptionalCredential }
    };
    // Params-only fold — the coherence input before the declaration folds in.
    let mut param_fold = quote! { ::axotyped::OptionalCredential };
    for p in &custom_params {
        let prev = param_fold;
        param_fold = quote! {
            <#prev as ::axotyped::AuthOr<<#p as ::axotyped::EndpointParam>::Auth>>::Out
        };
        let prev = authz;
        authz = quote! {
            <#prev as ::axotyped::AuthOr<<#p as ::axotyped::EndpointParam>::Auth>>::Out
        };
    }

    // Definition-site coherence: declaring a credentialless visibility
    // (public/permissive) while some parameter extractor requires a
    // credential is a contradiction — the extractor rejects the anonymous
    // callers the declaration promises to serve. Fails to compile at THIS
    // endpoint, with the trait names carrying the explanation.
    let non_private = is_public || is_permissive;
    let coherence_impl = quote! {
        #[doc(hidden)]
        impl ::axotyped::EndpointVisibilityCoherence for #meta_struct_name where
            ::axotyped::AssertCredentiallessDeclaration<{
                !#non_private
                    || !<#param_fold as ::axotyped::FlagConst>::IS_CREDENTIAL
            }>: ::axotyped::IsCoherentWithParams,
        {}
    };

    let body_register = match &body_type {
        Some(t) => {
            let inner = unwrap_container_types(t);
            quote! {
                __def.body_type = Some(::axotyped::__private::type_string::<#t>());
                __c.register::<#t>();
                #(__c.register::<#inner>();)*
            }
        }
        None => quote! {},
    };

    let query_register = match &query_type {
        Some(t) => {
            let inner = unwrap_container_types(t);
            quote! {
                __def.query_type = Some(::axotyped::__private::type_string::<#t>());
                __c.register::<#t>();
                #(__c.register::<#inner>();)*
            }
        }
        None => quote! {},
    };

    let response_register = match &response_type {
        Some(t) => {
            let inner = unwrap_container_types(t);
            quote! {
                __def.response_type = Some(::axotyped::__private::type_string::<#t>());
                __c.register::<#t>();
                #(__c.register::<#inner>();)*
            }
        }
        None => quote! {},
    };

    // Visibility declarations record both the declared and (for now)
    // effective state; an auth layer in scope later forces effective back to
    // `Private` (server enforcement is the ground truth). Redirect-following
    // is likewise declared here, never caller-supplied.
    let visibility_stmt = if is_public {
        quote! {
            __def.declared = ::axotyped::Visibility::Public;
            __def.visibility = ::axotyped::Visibility::Public;
        }
    } else if is_permissive {
        quote! {
            __def.declared = ::axotyped::Visibility::Permissive;
            __def.visibility = ::axotyped::Visibility::Permissive;
        }
    } else {
        quote! {}
    };
    let redirects_stmt = if allow_redirects {
        quote! { __def.allow_redirects = true; }
    } else {
        quote! {}
    };

    let expanded = quote! {
        // Original function unchanged
        #item_fn

        // Companion metadata struct — zero-sized, no runtime cost
        #[allow(non_camel_case_types)]
        pub struct #meta_struct_name;

        #param_assertions
        #coherence_impl

        impl ::axotyped::EndpointMeta for #meta_struct_name {
            type Authorization = #authz;
            type ParamCredentials = #param_fold;

            fn apply<C: ::axotyped::Collector>(__def: &mut ::axotyped::RouteDefinition, __c: &mut C) {
                #body_register
                #query_register
                #response_register
                #visibility_stmt
                #redirects_stmt
            }
        }
    };

    TokenStream::from(expanded)
}

fn endpoint_attr_error() -> TokenStream {
    quote! {
        compile_error!(
            "axotyped: #[endpoint] accepts at most a visibility (`public` | `permissive`) \
             and `allow_redirects` — e.g. #[endpoint(public, allow_redirects)]"
        );
    }
    .into()
}

// ---------------------------------------------------------------------------
// #[enforced] / #[ambient] — extractor credential classification
// ---------------------------------------------------------------------------

/// Mark an extractor as requiring authentication: a request without valid
/// credentials cannot reach the handler body through it (extraction fails
/// or rejects). Replaces the manual
/// `impl EndpointParam { type Auth = Credential }` boilerplate; the claim
/// sits on the type that performs the check.
///
/// ```rust,ignore
/// #[axotyped::credential]
/// pub struct AuthUser(pub UserIdentity);
/// ```
#[proc_macro_attribute]
pub fn credential(_attr: TokenStream, item: TokenStream) -> TokenStream {
    credential_marker(item, quote! { ::axotyped::Credential })
}

/// Mark an extractor as allowing anonymous requests: extraction succeeds
/// without credentials and yields no identity (e.g. `OptionalAuthUser`
/// returning `None`). Explicit form of the default classification for
/// custom extractors.
#[proc_macro_attribute]
pub fn optional_credential(_attr: TokenStream, item: TokenStream) -> TokenStream {
    credential_marker(item, quote! { ::axotyped::OptionalCredential })
}

fn credential_marker(item: TokenStream, flag: proc_macro2::TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as syn::DeriveInput);
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    quote! {
        #input

        impl #impl_generics ::axotyped::EndpointParam for #name #ty_generics #where_clause {
            type Auth = #flag;
        }
    }
    .into()
}

// ---------------------------------------------------------------------------
// register!() — call-site macro that registers a handler with its metadata
// ---------------------------------------------------------------------------

/// Wrap a handler with its auto-inferred `#[endpoint]` metadata.
///
/// Expands to a [`Registered`](axotyped::Registered) value carrying the handler and its
/// `EndpointMeta` at the type level. Pass it to `.post()`, `.get()`, etc.; the builder applies
/// the inferred body/response/query types through its collector.
///
/// Requires the handler function to be annotated with `#[endpoint]`.
///
/// # Example
///
/// ```rust,ignore
/// .post("/course", register!(api::admin::create_course))
///     .auth()
///     .done()
/// ```
#[proc_macro]
pub fn register(input: TokenStream) -> TokenStream {
    let path: syn::Path = parse_macro_input!(input as syn::Path);

    // Derive the metadata path by appending __EndpointMeta to the last segment
    let mut meta_path = path.clone();
    if let Some(last) = meta_path.segments.last_mut() {
        let original = &last.ident;
        let meta_ident = quote::format_ident!("{}__EndpointMeta", original);
        last.ident = meta_ident;
    }

    let expanded = quote! {
        ::axotyped::Registered::<_, #meta_path>::new(#path)
    };

    TokenStream::from(expanded)
}

// ---------------------------------------------------------------------------
// Type extraction helpers
// ---------------------------------------------------------------------------

/// Check whether a type can be used in generated turbofish positions.
///
/// `impl Trait` types (e.g. `impl IntoResponse`) cannot appear in
/// `::<T>()` turbofish syntax, so we must exclude them from metadata generation.
fn is_type_inferable(ty: &Type) -> bool {
    !matches!(ty, Type::ImplTrait(_))
}

/// Last path segment identifier of a type ("State" for `axum::extract::State<T>`);
/// empty for non-path types.
fn last_segment_ident(ty: &Type) -> String {
    let Type::Path(type_path) = ty else {
        return String::new();
    };
    type_path
        .path
        .segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_default()
}

/// Extract the inner `T` from `Json<T>` in function parameters.
fn extract_body_type(item_fn: &ItemFn) -> Option<Type> {
    for arg in &item_fn.sig.inputs {
        if let FnArg::Typed(pat_type) = arg {
            if let Some(inner) = extract_generic_from_type(&pat_type.ty, "Json") {
                if is_type_inferable(inner) {
                    return Some(inner.clone());
                }
            }
        }
    }
    None
}

/// Extract the inner `T` from `Query<T>` in function parameters.
fn extract_query_type(item_fn: &ItemFn) -> Option<Type> {
    for arg in &item_fn.sig.inputs {
        if let FnArg::Typed(pat_type) = arg {
            if let Some(inner) = extract_generic_from_type(&pat_type.ty, "Query") {
                if is_type_inferable(inner) {
                    return Some(inner.clone());
                }
            }
        }
    }
    None
}

/// Extract the response type from the return type.
///
/// Handles:
/// - `Json<T>` → T
/// - `Result<Json<T>, E>` → T
/// - `Result<StatusCode, StatusCode>` → None (no response body)
/// - `Result<impl Trait, E>` → None (cannot use in turbofish)
fn extract_response_type(item_fn: &ItemFn) -> Option<Type> {
    let ret = match &item_fn.sig.output {
        ReturnType::Type(_, ty) => ty.as_ref(),
        ReturnType::Default => return None,
    };

    // Try to unwrap Result<Json<T>, E>
    if let Some(ok_type) = extract_generic_from_type(ret, "Result") {
        // impl Trait cannot be placed in turbofish positions
        if !is_type_inferable(ok_type) {
            return None;
        }
        if let Some(inner) = extract_generic_from_type(ok_type, "Json") {
            return Some(inner.clone());
        }
        // Result<StatusCode, StatusCode> — no response body
        if is_status_code(ok_type) {
            return None;
        }
        // Result<Response, _> — redirect/plain response (cookie + redirect
        // callbacks, files): no JSON response type for the client.
        if last_segment_ident(ok_type) == "Response" {
            return None;
        }
        return Some(ok_type.clone());
    }

    // Try Json<T> directly (non-Result)
    if let Some(inner) = extract_generic_from_type(ret, "Json") {
        if is_type_inferable(inner) {
            return Some(inner.clone());
        }
    }

    None
}

/// Check if a type is `StatusCode`.
fn is_status_code(ty: &Type) -> bool {
    let Type::Path(type_path) = ty else {
        return false;
    };
    type_path
        .path
        .segments
        .last()
        .is_some_and(|s| s.ident == "StatusCode")
}

/// Extract the first generic argument from a type like `Wrapper<Inner>`.
fn extract_generic_from_type<'a>(ty: &'a Type, wrapper: &str) -> Option<&'a Type> {
    let Type::Path(type_path) = ty else {
        return None;
    };

    let seg = type_path.path.segments.last()?;
    if seg.ident != wrapper {
        return None;
    }

    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };

    match args.args.first()? {
        syn::GenericArgument::Type(inner) => Some(inner),
        _ => None,
    }
}


/// Extractor names with `EndpointParam` blanket impls in axotyped — skipped
/// by the authorization fold (their flag is `Ambient` by construction).
/// Must stay in sync with `src/params.rs`. Skipping matters for privacy:
/// the fold lands in the public `EndpointMeta::Authorization` associated
/// type, which must not mention private DTOs riding inside `Json<T>` and
/// friends; only custom (necessarily public) extractors fold.
const KNOWN_AMBIENT_EXTRACTORS: &[&str] = &[
    "State", "Json", "Query", "Path", "Extension", "HeaderMap", "Request",
    "RawQuery", "MatchedPath", "OriginalUri", "ConnectInfo", "WebSocketUpgrade",
    "Option",
];

/// All typed parameter types of the handler — every extractor in an
/// `#[endpoint]` signature must implement `EndpointParam` (blanket impls
/// cover axum's built-ins; custom extractors declare whether they enforce
/// authentication).
fn extract_param_types(item_fn: &ItemFn) -> Vec<Type> {
    item_fn
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat_type) => Some(pat_type.ty.as_ref().clone()),
            FnArg::Receiver(_) => None,
        })
        .collect()
}

/// Recursively unwrap `Vec<T>` and `Option<T>` wrappers, returning the
/// innermost non-container type(s). This ensures types that only appear
/// inside a container (e.g. `Vec<SkillProgress>`) still get registered.
fn unwrap_container_types(ty: &Type) -> Vec<Type> {
    let mut current = ty;
    loop {
        if let Some(inner) = extract_generic_from_type(current, "Vec") {
            current = inner;
        } else if let Some(inner) = extract_generic_from_type(current, "Option") {
            current = inner;
        } else {
            return vec![current.clone()];
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_extract_json_body() {
        let item_fn: ItemFn = parse_quote! {
            pub async fn create_user(
                State(state): State<AppState>,
                Json(body): Json<CreateUserRequest>,
            ) -> Result<Json<User>, StatusCode> {}
        };
        let ty = extract_body_type(&item_fn).unwrap();
        if let Type::Path(p) = &ty {
            assert_eq!(p.path.segments.last().unwrap().ident, "CreateUserRequest");
        }
    }

    #[test]
    fn test_extract_result_json_response() {
        let item_fn: ItemFn = parse_quote! {
            pub async fn get_user(
                Path(id): Path<String>,
            ) -> Result<Json<User>, StatusCode> {}
        };
        let ty = extract_response_type(&item_fn).unwrap();
        if let Type::Path(p) = &ty {
            assert_eq!(p.path.segments.last().unwrap().ident, "User");
        }
    }

    #[test]
    fn test_extract_plain_json_response() {
        let item_fn: ItemFn = parse_quote! {
            pub async fn health() -> Json<HealthResponse> {}
        };
        let ty = extract_response_type(&item_fn).unwrap();
        if let Type::Path(p) = &ty {
            assert_eq!(p.path.segments.last().unwrap().ident, "HealthResponse");
        }
    }

    #[test]
    fn test_no_response_for_status_code() {
        let item_fn: ItemFn = parse_quote! {
            pub async fn delete_user(Path(id): Path<String>) -> Result<StatusCode, StatusCode> {}
        };
        assert!(extract_response_type(&item_fn).is_none());
    }

    #[test]
    fn test_no_body_when_none() {
        let item_fn: ItemFn = parse_quote! {
            pub async fn list_users() -> Result<Json<Vec<User>>, StatusCode> {}
        };
        assert!(extract_body_type(&item_fn).is_none());
    }

    #[test]
    fn test_extract_query_type() {
        let item_fn: ItemFn = parse_quote! {
            pub async fn search(
                Query(params): Query<SearchParams>,
            ) -> Result<Json<Vec<User>>, StatusCode> {}
        };
        let ty = extract_query_type(&item_fn).unwrap();
        if let Type::Path(p) = &ty {
            assert_eq!(p.path.segments.last().unwrap().ident, "SearchParams");
        }
    }

    #[test]
    fn test_unwrap_vec_response() {
        let item_fn: ItemFn = parse_quote! {
            pub async fn list_users() -> Result<Json<Vec<User>>, StatusCode> {}
        };
        let ty = extract_response_type(&item_fn).unwrap();
        // Response type should be Vec<User>
        if let Type::Path(p) = &ty {
            assert_eq!(p.path.segments.last().unwrap().ident, "Vec");
        }
        // Inner types should include User
        let inner = unwrap_container_types(&ty);
        assert_eq!(inner.len(), 1);
        if let Type::Path(p) = &inner[0] {
            assert_eq!(p.path.segments.last().unwrap().ident, "User");
        }
    }

    #[test]
    fn test_unwrap_nested_option_vec() {
        let item_fn: ItemFn = parse_quote! {
            pub async fn get_items() -> Result<Json<Option<Vec<Item>>>, StatusCode> {}
        };
        let ty = extract_response_type(&item_fn).unwrap();
        let inner = unwrap_container_types(&ty);
        assert_eq!(inner.len(), 1);
        if let Type::Path(p) = &inner[0] {
            assert_eq!(p.path.segments.last().unwrap().ident, "Item");
        }
    }

    #[test]
    fn test_unwrap_plain_type_unchanged() {
        let item_fn: ItemFn = parse_quote! {
            pub async fn get_user() -> Result<Json<User>, StatusCode> {}
        };
        let ty = extract_response_type(&item_fn).unwrap();
        let inner = unwrap_container_types(&ty);
        assert_eq!(inner.len(), 1);
        if let Type::Path(p) = &inner[0] {
            assert_eq!(p.path.segments.last().unwrap().ident, "User");
        }
    }

    #[test]
    fn test_reject_impl_trait_response() {
        // Result<impl IntoResponse, StatusCode> must not produce metadata
        let item_fn: ItemFn = parse_quote! {
            pub async fn handler() -> Result<impl IntoResponse, StatusCode> {}
        };
        assert!(extract_response_type(&item_fn).is_none());
    }

    #[test]
    fn test_reject_impl_trait_body() {
        // Json<impl DeserializeOwned> in params must not produce metadata
        let item_fn: ItemFn = parse_quote! {
            pub async fn handler(Json(body): Json<impl DeserializeOwned>) -> StatusCode {}
        };
        assert!(extract_body_type(&item_fn).is_none());
    }

    #[test]
    fn test_reject_impl_trait_query() {
        // Query<impl SomeTrait> in params must not produce metadata
        let item_fn: ItemFn = parse_quote! {
            pub async fn handler(Query(params): Query<impl SomeTrait>) -> StatusCode {}
        };
        assert!(extract_query_type(&item_fn).is_none());
    }

    #[test]
    fn test_reject_impl_trait_in_json_response() {
        // -> Json<impl SomeTrait> (non-Result) must not produce metadata
        let item_fn: ItemFn = parse_quote! {
            pub async fn handler() -> Json<impl Serialize> {}
        };
        assert!(extract_response_type(&item_fn).is_none());
    }
}
