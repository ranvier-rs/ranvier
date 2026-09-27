use proc_macro2::TokenStream;
use quote::ToTokens;
use std::collections::HashSet;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, Type};

#[derive(Default)]
pub(crate) struct TransitionArgs {
    pub(crate) resources: Option<Type>,
    pub(crate) resources_span: Option<proc_macro2::Span>,
    pub(crate) bus_allow: Vec<Type>,
    pub(crate) bus_deny: Vec<Type>,
    pub(crate) bus_allow_specified: bool,
    pub(crate) bus_deny_specified: bool,
    pub(crate) bus_allow_span: Option<proc_macro2::Span>,
    pub(crate) bus_deny_span: Option<proc_macro2::Span>,
    pub(crate) schema: bool,
    pub(crate) schema_span: Option<proc_macro2::Span>,
}

impl TransitionArgs {
    pub(crate) fn parse(tokens: TokenStream) -> syn::Result<Self> {
        syn::parse2(tokens)
    }

    #[cfg(feature = "streaming")]
    pub(crate) fn validate_streaming_metadata(&self) -> syn::Result<()> {
        let mut errors = None;
        if self.bus_allow_specified || self.bus_deny_specified {
            if let Some(span) = self.bus_allow_span {
                errors = Some(syn::Error::new(
                    span,
                    "streaming transitions do not expose Bus-policy metadata",
                ));
            }
            if let Some(span) = self.bus_deny_span {
                let error = syn::Error::new(
                    span,
                    "streaming transitions do not expose Bus-policy metadata",
                );
                if let Some(errors) = &mut errors {
                    errors.combine(error);
                } else {
                    errors = Some(error);
                }
            }
        }
        if self.schema {
            let span = self
                .schema_span
                .unwrap_or_else(proc_macro2::Span::call_site);
            let error = syn::Error::new(
                span,
                "streaming item schema metadata is not implemented yet",
            );
            if let Some(errors) = &mut errors {
                errors.combine(error);
            } else {
                errors = Some(error);
            }
        }
        errors.map_or(Ok(()), Err)
    }
}

impl Parse for TransitionArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut parsed = Self::default();

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            let key_span = key.span();
            match key.to_string().as_str() {
                "schema" => {
                    if parsed.schema {
                        return Err(syn::Error::new_spanned(key, "duplicate `schema` attribute"));
                    }
                    parsed.schema = true;
                    parsed.schema_span = Some(key_span);
                }
                "resources" => {
                    if parsed.resources.is_some() {
                        return Err(syn::Error::new_spanned(
                            key,
                            "duplicate `resources` attribute",
                        ));
                    }
                    input.parse::<syn::Token![=]>()?;
                    parsed.resources = Some(input.parse::<Type>().map_err(|_| {
                        syn::Error::new(key_span, "`resources` expects a Rust type")
                    })?);
                    parsed.resources_span = Some(key_span);
                }
                "bus_allow" => {
                    if parsed.bus_allow_specified {
                        return Err(syn::Error::new_spanned(
                            key,
                            "duplicate `bus_allow` attribute",
                        ));
                    }
                    parsed.bus_allow_specified = true;
                    parsed.bus_allow_span = Some(key_span);
                    input.parse::<syn::Token![=]>()?;
                    parsed.bus_allow = parse_type_array(input)?;
                }
                "bus_deny" => {
                    if parsed.bus_deny_specified {
                        return Err(syn::Error::new_spanned(
                            key,
                            "duplicate `bus_deny` attribute",
                        ));
                    }
                    parsed.bus_deny_specified = true;
                    parsed.bus_deny_span = Some(key_span);
                    input.parse::<syn::Token![=]>()?;
                    parsed.bus_deny = parse_type_array(input)?;
                }
                "res" | "x" | "y" => {
                    return Err(syn::Error::new_spanned(
                        &key,
                        format!("unsupported transition attribute `{key}`"),
                    ));
                }
                _ => {
                    return Err(syn::Error::new_spanned(
                        key,
                        "unknown or malformed transition attribute; expected `schema`, `resources = Type`, `bus_allow = [Type, ...]`, or `bus_deny = [Type, ...]`",
                    ));
                }
            }

            if input.is_empty() {
                break;
            }
            input.parse::<syn::Token![,]>()?;
        }

        validate_bus_policy_types(&parsed.bus_allow, &parsed.bus_deny)?;
        Ok(parsed)
    }
}

fn parse_type_array(input: ParseStream<'_>) -> syn::Result<Vec<Type>> {
    let content;
    syn::bracketed!(content in input);
    Ok(
        syn::punctuated::Punctuated::<Type, syn::Token![,]>::parse_terminated(&content)?
            .into_iter()
            .collect(),
    )
}

fn validate_bus_policy_types(allow: &[Type], deny: &[Type]) -> syn::Result<()> {
    let mut allow_keys = HashSet::new();
    for ty in allow {
        let key = ty.to_token_stream().to_string();
        if !allow_keys.insert(key) {
            return Err(syn::Error::new_spanned(
                ty,
                "duplicate type in bus_allow list",
            ));
        }
    }

    let mut deny_keys = HashSet::new();
    for ty in deny {
        let key = ty.to_token_stream().to_string();
        if !deny_keys.insert(key) {
            return Err(syn::Error::new_spanned(
                ty,
                "duplicate type in bus_deny list",
            ));
        }
    }

    for ty in allow {
        let key = ty.to_token_stream().to_string();
        if deny_keys.contains(&key) {
            return Err(syn::Error::new_spanned(
                ty,
                "same type cannot be present in both bus_allow and bus_deny",
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::TransitionArgs;
    use quote::{ToTokens, quote};

    #[test]
    fn parses_clean_break_attributes() {
        let args = TransitionArgs::parse(quote!(
            schema,
            resources = AppResources,
            bus_allow = [AuthContext],
            bus_deny = [InternalToken]
        ))
        .expect("attributes should parse");

        assert!(args.schema);
        assert!(args.resources.is_some());
        assert_eq!(args.bus_allow.len(), 1);
        assert_eq!(args.bus_deny.len(), 1);
    }

    #[test]
    fn parses_generic_resource_type_as_a_type() {
        let args = TransitionArgs::parse(quote!(resources = std::sync::Arc<AppResources>))
            .expect("generic resource type should parse");
        let resources = args.resources.expect("resource type should exist");
        assert_eq!(
            resources.to_token_stream().to_string(),
            "std :: sync :: Arc < AppResources >"
        );
    }

    #[test]
    fn rejects_unknown_attribute() {
        let error = TransitionArgs::parse(quote!(scheam))
            .err()
            .expect("should reject typo");
        assert!(error.to_string().contains("unknown or malformed"));
    }

    #[test]
    fn rejects_removed_attribute() {
        let error = TransitionArgs::parse(quote!(res = AppResources))
            .err()
            .expect("should reject removed alias");
        assert!(
            error
                .to_string()
                .contains("unsupported transition attribute `res`")
        );
    }

    #[test]
    fn rejects_duplicate_attribute() {
        let error = TransitionArgs::parse(quote!(resources = A, resources = B))
            .err()
            .expect("should reject duplicate");
        assert!(error.to_string().contains("duplicate `resources`"));
    }

    #[test]
    fn rejects_bus_policy_overlap() {
        let error = TransitionArgs::parse(quote!(bus_allow = [Auth], bus_deny = [Auth]))
            .err()
            .expect("should reject overlap");
        assert!(error.to_string().contains("both bus_allow and bus_deny"));
    }
}
