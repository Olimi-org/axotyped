use proc_macro2::Span;
use syn::{
    Error, Expr, Ident, Path, Token, Type, ext::IdentExt, parse::ParseStream, parse_quote,
    parse_quote_spanned,
};

use crate::ts::attr::FieldAttr;

/// Indicates whether the field is marked with `#[ts(optional)]`.
/// `#[ts(optional)]` turns an `t: Option<T>` into `t?: T`, while
/// `#[ts(optional = nullable)]` turns it into `t?: T | null`.
#[derive(Default, Clone, Copy)]
pub enum Optional {
    /// Explicitly marked as optional with `#[ts(optional)]`
    #[allow(clippy::enum_variant_names)]
    Optional { nullable: bool },

    /// Explicitly marked as not optional with `#[ts(optional = false)]`
    #[allow(clippy::enum_variant_names)]
    NotOptional,

    #[default]
    Inherit,
}

impl Optional {
    pub fn or(self, other: Optional) -> Self {
        match (self, other) {
            (Self::Inherit, other) | (other, Self::Inherit) => other,
            (Self::Optional { nullable: a }, Self::Optional { nullable: b }) => {
                Self::Optional { nullable: a || b }
            }
            _ => other,
        }
    }
}

pub fn parse_optional(input: ParseStream) -> syn::Result<Optional> {
    let optional = if input.peek(Token![=]) {
        input.parse::<Token![=]>()?;
        let span = input.span();

        match Ident::parse_any(input)?.to_string().as_str() {
            "nullable" => Optional::Optional { nullable: true },
            "false" => Optional::NotOptional,
            _ => Err(Error::new(span, "expected 'nullable'"))?,
        }
    } else {
        Optional::Optional { nullable: false }
    };

    Ok(optional)
}

/// Returns `(is_optional, type)` for a field.
/// `is_optional` emits `?`; `type` is the field type after applying
/// `#[ts(optional)]` (`Option<T>` becomes `T`, or `T | null` with `nullable`).
pub fn apply(
    crate_rename: &Path,
    for_struct: Optional,
    field_ty: &Type,
    attr: &FieldAttr,
    span: Span,
) -> (Expr, Type) {
    match (for_struct, attr.optional) {
        // explicit `#[ts(optional = false)]` on field, or inherited from struct.
        (Optional::NotOptional, Optional::Inherit) | (_, Optional::NotOptional) => {
            (parse_quote!(false), field_ty.clone())
        }
        // Explicit `#[ts(optional)]` on field; takes precedence over struct-level.
        (_, Optional::Optional { nullable }) => (
            parse_quote!(true),
            if nullable {
                field_ty.clone()
            } else {
                // Inner type of `Option`; fails to compile on non-`Option`.
                parse_quote_spanned! {
                    span => <#field_ty as #crate_rename::IsOption>::Inner
                }
            },
        ),
        // Inherited `#[ts(optional)]` from struct; no-op on non-`Option` fields.
        (Optional::Optional { nullable }, Optional::Inherit) if attr.type_override.is_none() => (
            parse_quote! {
                <#field_ty as #crate_rename::TS>::IS_OPTION
            },
            if nullable {
                field_ty.clone()
            } else {
                unwrap_option(crate_rename, field_ty)
            },
        ),
        // no applicable `#[ts(optional)]` attributes
        _ => {
            // field may be omitted during serialization and has a default value, so the field can be
            // treated as `#[ts(optional = nullable)]`.
            let is_optional = attr.maybe_omitted && attr.has_default;
            // With `#[ts(type)]` the field type is unused for rendering,
            // so skip the `IS_OPTION` probe to avoid requiring `T: TS`.
            if attr.type_override.is_some() {
                return (parse_quote!(#is_optional), field_ty.clone());
            }
            // `Option` fields accept a missing key as `None`; the type keeps
            // `| null` for responses that serialize `None`.
            (
                parse_quote!((#is_optional) || <#field_ty as #crate_rename::TS>::IS_OPTION),
                field_ty.clone(),
            )
        }
    }
}

/// Unwraps `Option<T>` to `T`; returns other types as-is.
fn unwrap_option(crate_rename: &Path, ty: &Type) -> Type {
    parse_quote! {<#ty as #crate_rename::TS>::OptionInnerType}
}

/// Matches `Option<T>` by final path segment with one generic arg.
pub(crate) fn is_option_ty(ty: &Type) -> bool {
    let Type::Path(tp) = ty else { return false };
    let Some(seg) = tp.path.segments.last() else {
        return false;
    };
    if seg.ident != "Option" {
        return false;
    }
    matches!(
        &seg.arguments,
        syn::PathArguments::AngleBracketed(args) if args.args.len() == 1
    )
}
