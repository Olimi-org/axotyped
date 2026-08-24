//! Proc-macros for axotyped route type inference.
//!
//! - `#[endpoint]` — attribute on handler functions; extracts `Json<T>`, `Query<T>` from
//!   the signature and generates a companion `__EndpointMeta` struct
//! - `register!()` — call-site macro that wraps a handler path with its metadata
//! - `#[derive(TS)]` — (behind `ts-rs` feature) generate TypeScript bindings from Rust types

#[cfg(feature = "ts-rs")]
mod ts;

// TS derive macro — must reside in the crate root.
#[cfg(feature = "ts-rs")]
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
/// `#[endpoint(public)]` to mark an individual handler's route public (the
/// scope-wide alternative is [`ApiRouter::group_public`] / `group_public` on
/// the builder).
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

    // Options: `#[endpoint]` or `#[endpoint(public)]` — anything else is a
    // compile error so typos can never silently flip visibility.
    let mut is_public = false;
    let attr_tts: Vec<proc_macro::TokenTree> = attr.into_iter().collect();
    match attr_tts.as_slice() {
        [] => {}
        [proc_macro::TokenTree::Ident(id)] if id.to_string() == "public" => is_public = true,
        _ => {
            return quote! {
                compile_error!(
                    "axotyped: #[endpoint] accepts no arguments or exactly `public` \
                     — e.g. #[endpoint(public)]"
                );
            }
            .into();
        }
    }

    let fn_name = &item_fn.sig.ident;
    let meta_struct_name = quote::format_ident!("{}__EndpointMeta", fn_name);

    let body_type = extract_body_type(&item_fn);
    let query_type = extract_query_type(&item_fn);
    let response_type = extract_response_type(&item_fn);

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

    // Only `#[endpoint(public)]` opens a route.
    let visibility_stmt = if is_public {
        quote! { __def.auth = false; }
    } else {
        quote! {}
    };

    let expanded = quote! {
        // Original function unchanged
        #item_fn

        // Companion metadata struct — zero-sized, no runtime cost
        #[allow(non_camel_case_types)]
        pub struct #meta_struct_name;

        impl ::axotyped::EndpointMeta for #meta_struct_name {
            fn apply<C: ::axotyped::Collector>(__def: &mut ::axotyped::RouteDefinition, __c: &mut C) {
                #body_register
                #query_register
                #response_register
                #visibility_stmt
            }
        }
    };

    TokenStream::from(expanded)
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
