use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{DeriveInput, ItemFn, parse_macro_input};

mod config;
mod signature;
#[cfg(feature = "streaming")]
mod streaming;

use config::TransitionArgs;
use signature::TransitionFunction;

fn external_crate_path(package: &str) -> Option<TokenStream2> {
    match crate_name(package).ok()? {
        FoundCrate::Itself => Some(quote!(crate)),
        FoundCrate::Name(name) => {
            let ident = syn::Ident::new(&name, Span::call_site());
            Some(quote!(::#ident))
        }
    }
}

pub(crate) fn core_crate_path() -> syn::Result<TokenStream2> {
    if let Some(path) = external_crate_path("ranvier-core") {
        return Ok(path);
    }
    if let Some(facade) = external_crate_path("ranvier") {
        return Ok(quote!(#facade::core));
    }
    Err(syn::Error::new(
        Span::call_site(),
        "Ranvier macro expansion requires a direct `ranvier-core` dependency or the `ranvier` facade",
    ))
}

/// Attribute macro to transform an async function into a `Transition` implementation.
#[proc_macro_attribute]
pub fn transition(attr: TokenStream, item: TokenStream) -> TokenStream {
    let core_path = match core_crate_path() {
        Ok(path) => path,
        Err(error) => return error.to_compile_error().into(),
    };

    expand_transition(core_path, attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_transition(
    core_path: TokenStream2,
    attr: TokenStream2,
    item: TokenStream2,
) -> syn::Result<TokenStream2> {
    let args = TransitionArgs::parse(attr)?;
    let function = TransitionFunction::parse(syn::parse2::<ItemFn>(item)?, &args)?;
    let transition_type = quote::format_ident!("__ranvier_transition_{}", function.name);
    let name = &function.name;
    let visibility = &function.visibility;
    let shared_attributes = &function.shared_attributes;
    let documentation = &function.documentation;
    let input_type = &function.input_type;
    let resource_type = &function.resource_type;
    let bindings = &function.bindings;
    let output_type = &function.output_type;
    let error_type = &function.error_type;
    let body = &function.body;
    let bus_allow_types = &args.bus_allow;
    let bus_deny_types = &args.bus_deny;

    let bus_policy_method = if args.bus_allow_specified || args.bus_deny_specified {
        let allow_expr = if args.bus_allow_specified {
            quote! {
                Some(vec![#(#core_path::bus::BusTypeRef::of::<#bus_allow_types>()),*])
            }
        } else {
            quote! { None }
        };
        let deny_expr = if args.bus_deny_specified {
            quote! {
                vec![#(#core_path::bus::BusTypeRef::of::<#bus_deny_types>()),*]
            }
        } else {
            quote! { Vec::new() }
        };
        quote! {
            fn bus_access_policy(&self) -> Option<#core_path::bus::BusAccessPolicy> {
                Some(#core_path::bus::BusAccessPolicy {
                    allow: #allow_expr,
                    deny: #deny_expr,
                })
            }
        }
    } else {
        quote! {}
    };

    let schema_method = if args.schema {
        quote! {
            fn input_schema(&self) -> Option<#core_path::__macro_support::serde_json::Value> {
                let schema = #core_path::__macro_support::schemars::schema_for!(#input_type);
                #core_path::__macro_support::serde_json::to_value(schema).ok()
            }
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        #(#shared_attributes)*
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        #[derive(Clone)]
        #visibility struct #transition_type;

        #(#shared_attributes)*
        #(#documentation)*
        #[allow(non_upper_case_globals)]
        #visibility const #name: #transition_type = #transition_type;

        #(#shared_attributes)*
        #[#core_path::__macro_support::async_trait]
        impl #core_path::transition::Transition<#input_type, #output_type> for #transition_type {
            type Error = #error_type;
            type Resources = #resource_type;

            #bus_policy_method
            #schema_method

            async fn run(
                &self,
                input: #input_type,
                resources: &Self::Resources,
                bus: &mut #core_path::bus::Bus,
            ) -> #core_path::outcome::Outcome<#output_type, Self::Error> {
                #bindings
                #body
            }
        }
    };

    Ok(expanded)
}

/// Attribute macro for HTTP route registration.
#[proc_macro_attribute]
pub fn route(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);
    let original_ident = input_fn.sig.ident.clone();
    let vis = &input_fn.vis;

    let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
    let attr_args = parse_macro_input!(attr with parser);

    if attr_args.len() < 2 {
        return TokenStream::from(
            quote! { compile_error!("route attribute requires method and path"); },
        );
    }

    let method = &attr_args[0];
    let path = &attr_args[1];

    // For routes, we keep the function name for the function, and use a prefix for the metadata struct.
    let struct_name = quote::format_ident!("Route_{}", original_ident);

    let expanded = quote! {
        #input_fn

        #[allow(non_camel_case_types)]
        #vis struct #struct_name;

        impl #struct_name {
            pub const METHOD: &'static str = stringify!(#method);
            pub const PATH: &'static str = #path;
        }
    };

    TokenStream::from(expanded)
}

/// Macro to build a router from a list of circuit functions annotated with `#[route]`.
#[proc_macro]
pub fn ranvier_router(input: TokenStream) -> TokenStream {
    let parser = syn::punctuated::Punctuated::<syn::Ident, syn::Token![,]>::parse_terminated;
    let idents = parse_macro_input!(input with parser);

    let mut registrations = quote! {};

    for ident in idents {
        let route_struct = quote::format_ident!("Route_{}", ident);
        registrations.extend(quote! {
            let method_str = #route_struct::METHOD;
            let method = match method_str {
                "GET" => http::Method::GET,
                "POST" => http::Method::POST,
                "PUT" => http::Method::PUT,
                "DELETE" => http::Method::DELETE,
                _ => http::Method::GET,
            };
            ingress = ingress.route_method(method, #route_struct::PATH, #ident().await);
        });
    }

    let expanded = quote! {
        {
            let mut ingress = ranvier_http::HttpIngress::new();
            #registrations
            ingress
        }
    };

    TokenStream::from(expanded)
}

/// Attribute macro to transform an async function into a `StreamingTransition` implementation.
///
/// The function must return `Result<impl Stream<Item = T> + Send, E>`.
/// The macro generates a zero-sized struct and `StreamingTransition` trait impl.
///
/// ## Parameter Binding
///
/// Follows the same pattern as `#[transition]`:
/// - 1 param: `(input)` — Resources = ()
/// - 2 params: `(input, &Resources)` or `(input, &mut Bus)` — auto-detected
/// - 3 params: `(input, &Resources, &mut Bus)` — full form
///
/// ## Attributes
///
/// - `resources = MyType` — explicit resource type override
///
/// # Example
///
/// ```rust
/// use std::{pin::Pin, task::{Context, Poll}};
/// use ranvier_core::{
///     __macro_support::futures_core::Stream,
///     bus::Bus,
///     transition::ResourceRequirement as ResourceRequirementTrait,
/// };
/// use ranvier_macros::{streaming_transition, ResourceRequirement};
///
/// #[derive(Clone, ResourceRequirement)]
/// struct AppResources;
///
/// struct OneItemStream(Option<String>);
///
/// impl Stream for OneItemStream {
///     type Item = String;
///
///     fn poll_next(
///         mut self: Pin<&mut Self>,
///         _context: &mut Context<'_>,
///     ) -> Poll<Option<Self::Item>> {
///         Poll::Ready(self.0.take())
///     }
/// }
///
/// #[streaming_transition]
/// async fn synthesize(
///     input: String,
///     _resources: &AppResources,
///     _bus: &mut Bus,
/// ) -> Result<impl Stream<Item = String> + Send, String> {
///     Ok(OneItemStream(Some(input)))
/// }
///
/// fn assert_streaming_transition<T>(_: &T)
/// where
///     T: ranvier_core::streaming::StreamingTransition<
///         String,
///         Item = String,
///         Error = String,
///         Resources = AppResources,
///     > + Clone,
///     AppResources: ResourceRequirementTrait,
/// {
/// }
///
/// assert_streaming_transition(&synthesize);
/// ```
#[cfg(feature = "streaming")]
#[proc_macro_attribute]
pub fn streaming_transition(attr: TokenStream, item: TokenStream) -> TokenStream {
    match streaming::expand(attr.into(), item.into()) {
        Ok(expansion) => expansion.into(),
        Err(error) => error.into_compile_error().into(),
    }
}
/// Derive macro for the `ResourceRequirement` marker trait.
///
/// Generates a blanket `impl ResourceRequirement for YourType {}`.
/// The type must also implement `Clone` (required by the Axon execution engine).
///
/// # Example
///
/// ```rust
/// use ranvier_core::transition::ResourceRequirement as ResourceRequirementTrait;
/// use ranvier_macros::ResourceRequirement;
///
/// #[derive(Clone, ResourceRequirement)]
/// struct AppResources;
///
/// fn assert_resource_requirement<T: ResourceRequirementTrait>() {}
///
/// assert_resource_requirement::<AppResources>();
/// ```
#[proc_macro_derive(ResourceRequirement)]
pub fn derive_resource_requirement(input: TokenStream) -> TokenStream {
    let core_path = match core_crate_path() {
        Ok(path) => path,
        Err(error) => return error.to_compile_error().into(),
    };
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let expanded = quote! {
        impl #impl_generics #core_path::transition::ResourceRequirement for #name #ty_generics #where_clause {}
    };

    TokenStream::from(expanded)
}

#[cfg(test)]
mod ordinary_expansion_tests {
    use super::expand_transition;
    use proc_macro2::TokenStream as TokenStream2;
    use quote::{ToTokens, quote};
    use syn::{Attribute, Item, parse_quote};

    fn attribute_count(attributes: &[Attribute], expected: &Attribute) -> usize {
        let expected_tokens = expected.to_token_stream().to_string();
        attributes
            .iter()
            .filter(|attribute| attribute.to_token_stream().to_string() == expected_tokens)
            .count()
    }

    fn documentation_values(attributes: &[Attribute]) -> Vec<String> {
        attributes
            .iter()
            .filter_map(|attribute| {
                let syn::Meta::NameValue(documentation) = &attribute.meta else {
                    return None;
                };
                if !documentation.path.is_ident("doc") {
                    return None;
                }
                let syn::Expr::Lit(documentation) = &documentation.value else {
                    return None;
                };
                let syn::Lit::Str(documentation) = &documentation.lit else {
                    return None;
                };
                Some(documentation.value())
            })
            .collect()
    }

    #[test]
    fn copies_shared_attributes_to_each_generated_item() {
        let source = quote! {
            #[cfg(any(feature = "ordinary_attr_a", feature = "ordinary_attr_b"))]
            #[cfg_attr(all(), doc = "conditional ordinary transition documentation")]
            #[allow(dead_code)]
            /// Ordinary transition documentation.
            async fn attributed(
                input: i32,
            ) -> ranvier_core::outcome::Outcome<i32, std::convert::Infallible> {
                ranvier_core::outcome::Outcome::next(input)
            }
        };
        let expanded = expand_transition(quote!(::ranvier_core), TokenStream2::new(), source)
            .expect("valid ordinary transition function");
        let file = syn::parse2::<syn::File>(expanded).expect("generated ordinary items parse");
        assert_eq!(
            file.items.len(),
            3,
            "type, value, and trait impl are generated"
        );

        let [
            Item::Struct(transition_type),
            Item::Const(transition_value),
            Item::Impl(transition_impl),
        ] = file.items.as_slice()
        else {
            panic!("expected generated struct, const, and impl in order");
        };

        let expected_shared: [Attribute; 3] = [
            parse_quote!(#[cfg(any(feature = "ordinary_attr_a", feature = "ordinary_attr_b"))]),
            parse_quote!(#[cfg_attr(all(), doc = "conditional ordinary transition documentation")]),
            parse_quote!(#[allow(dead_code)]),
        ];
        let generated_attributes = [
            ("transition type", transition_type.attrs.as_slice()),
            ("transition value", transition_value.attrs.as_slice()),
            ("transition impl", transition_impl.attrs.as_slice()),
        ];

        for (generated_name, attributes) in generated_attributes {
            for expected_attribute in &expected_shared {
                assert_eq!(
                    attribute_count(attributes, expected_attribute),
                    1,
                    "{generated_name} must contain exactly one {:?} attribute",
                    expected_attribute.path(),
                );
            }
        }

        let source_documentation = " Ordinary transition documentation.";
        assert_eq!(
            documentation_values(&transition_type.attrs)
                .iter()
                .filter(|value| value.as_str() == source_documentation)
                .count(),
            0,
            "hidden type does not receive source docs",
        );
        assert_eq!(
            documentation_values(&transition_value.attrs)
                .iter()
                .filter(|value| value.as_str() == source_documentation)
                .count(),
            1,
            "named value receives the source doc exactly once",
        );
        assert_eq!(
            documentation_values(&transition_impl.attrs)
                .iter()
                .filter(|value| value.as_str() == source_documentation)
                .count(),
            0,
            "trait impl does not receive source docs",
        );
    }
}
