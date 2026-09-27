use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{DeriveInput, ItemFn, parse_macro_input};
#[cfg(feature = "streaming")]
use syn::{FnArg, GenericArgument, PathArguments, ReturnType, Type};

mod config;
mod signature;

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

fn core_crate_path() -> syn::Result<TokenStream2> {
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
    let args = match TransitionArgs::parse(attr.into()) {
        Ok(args) => args,
        Err(error) => return error.to_compile_error().into(),
    };
    let function = match TransitionFunction::parse(parse_macro_input!(item as ItemFn), &args) {
        Ok(function) => function,
        Err(error) => return error.to_compile_error().into(),
    };
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

    TokenStream::from(expanded)
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
/// ```rust,ignore
/// #[streaming_transition]
/// async fn synthesize(
///     input: ClassifiedChat,
///     resources: &AppResources,
///     bus: &mut Bus,
/// ) -> Result<impl Stream<Item = ChatChunk> + Send, LlmError> {
///     let stream = resources.llm.chat_stream(&input.prompt).await?;
///     Ok(stream)
/// }
/// ```
#[cfg(feature = "streaming")]
#[proc_macro_attribute]
pub fn streaming_transition(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input_fn = parse_macro_input!(item as ItemFn);
    let original_ident = input_fn.sig.ident.clone();
    let vis = &input_fn.vis;
    let block = &input_fn.block;
    let inputs = &input_fn.sig.inputs;

    let internal_fn_ident = quote::format_ident!("__ranvier_fn_{}", original_ident);
    input_fn.sig.ident = internal_fn_ident.clone();

    let args = match TransitionArgs::parse(attr.into()) {
        Ok(args) => args,
        Err(error) => return error.to_compile_error().into(),
    };
    if let Err(error) = args.validate_streaming_metadata() {
        return error.to_compile_error().into();
    }
    let res_override = args.resources;

    // 1. Extract Input Type (From)
    let input_type = if let Some(FnArg::Typed(pat_type)) = inputs.first() {
        let ty = &pat_type.ty;
        quote! { #ty }
    } else {
        quote! { () }
    };

    // 2. Extract Resources Type (same logic as #[transition])
    let second_is_bus = inputs.get(1).map(is_bus_argument).unwrap_or(false);
    let res_type = if let Some(res) = res_override {
        quote! { #res }
    } else if second_is_bus {
        quote! { () }
    } else if let Some(FnArg::Typed(pat_type)) = inputs.get(1) {
        let ty = &pat_type.ty;
        if let Type::Reference(type_ref) = &**ty {
            let elem = &type_ref.elem;
            quote! { #elem }
        } else {
            quote! { #ty }
        }
    } else {
        quote! { () }
    };

    // 3. Extract Stream Item type and Error type from Result<impl Stream<Item = T>, E>
    let (item_type, error_type) = if let ReturnType::Type(_, ty) = &input_fn.sig.output {
        extract_result_stream_types(ty).unwrap_or((quote! { () }, quote! { String }))
    } else {
        (quote! { () }, quote! { String })
    };

    // 4. Handle Arguments — produce only bindings (not the block)
    let arg_count = inputs.len();
    let bindings = match arg_count {
        1 => {
            if let Some(FnArg::Typed(pat_type)) = inputs.first() {
                let pat = &pat_type.pat;
                quote! { let #pat = input; }
            } else {
                quote! {}
            }
        }
        2 => {
            let mut b = quote! {};
            if let Some(FnArg::Typed(pat_type)) = inputs.first() {
                let pat = &pat_type.pat;
                b.extend(quote! { let #pat = input; });
            }
            if second_is_bus {
                if let Some(FnArg::Typed(pat_type)) = inputs.get(1) {
                    let pat = &pat_type.pat;
                    b.extend(quote! { let #pat = bus; });
                }
            } else if let Some(FnArg::Typed(pat_type)) = inputs.get(1) {
                let pat = &pat_type.pat;
                b.extend(quote! { let #pat = resources; });
            }
            b
        }
        3 => {
            let mut b = quote! {};
            if let Some(FnArg::Typed(pat_type)) = inputs.first() {
                let pat = &pat_type.pat;
                b.extend(quote! { let #pat = input; });
            }
            if let Some(FnArg::Typed(pat_type)) = inputs.get(1) {
                let pat = &pat_type.pat;
                b.extend(quote! { let #pat = resources; });
            }
            if let Some(FnArg::Typed(pat_type)) = inputs.get(2) {
                let pat = &pat_type.pat;
                b.extend(quote! { let #pat = bus; });
            }
            b
        }
        _ => quote! {},
    };

    let expanded = quote! {
        #[derive(Clone, Default)]
        #[allow(non_camel_case_types)]
        #vis struct #original_ident;

        #[::async_trait::async_trait]
        impl ranvier_core::streaming::StreamingTransition<#input_type> for #original_ident {
            type Item = #item_type;
            type Error = #error_type;
            type Resources = #res_type;

            async fn run_stream(
                &self,
                input: #input_type,
                resources: &Self::Resources,
                bus: &mut ranvier_core::bus::Bus,
            ) -> Result<
                std::pin::Pin<Box<dyn futures_core::Stream<Item = Self::Item> + Send>>,
                Self::Error,
            > {
                #bindings
                let __ranvier_stream_result = #block;
                __ranvier_stream_result.map(|__s| {
                    Box::pin(__s)
                        as std::pin::Pin<Box<dyn futures_core::Stream<Item = #item_type> + Send>>
                })
            }
        }

        #input_fn
    };

    TokenStream::from(expanded)
}

/// Extract (Item, Error) from `Result<impl Stream<Item = T> [+ Send], E>`.
#[cfg(feature = "streaming")]
fn extract_result_stream_types(
    ty: &Type,
) -> Option<(quote::__private::TokenStream, quote::__private::TokenStream)> {
    let Type::Path(type_path) = ty else {
        return None;
    };
    let segment = type_path.path.segments.last()?;
    if segment.ident != "Result" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    let mut iter = args.args.iter();
    let GenericArgument::Type(stream_ty) = iter.next()? else {
        return None;
    };
    let GenericArgument::Type(err_ty) = iter.next()? else {
        return None;
    };

    let item_ty = extract_stream_item_type(stream_ty)?;
    Some((item_ty, quote! { #err_ty }))
}

/// Extract the `Item` associated type from `impl Stream<Item = T>` or
/// `Pin<Box<dyn Stream<Item = T> + Send>>`.
#[cfg(feature = "streaming")]
fn extract_stream_item_type(ty: &Type) -> Option<quote::__private::TokenStream> {
    match ty {
        // Case 1: impl Stream<Item = T> [+ Send]
        Type::ImplTrait(impl_trait) => {
            for bound in &impl_trait.bounds {
                if let syn::TypeParamBound::Trait(trait_bound) = bound {
                    if let Some(item) = extract_item_from_stream_path(&trait_bound.path) {
                        return Some(item);
                    }
                }
            }
            None
        }
        // Case 2: Pin<Box<dyn Stream<Item = T> + Send>>
        Type::Path(type_path) => {
            let segment = type_path.path.segments.last()?;
            if segment.ident != "Pin" {
                return None;
            }
            let PathArguments::AngleBracketed(pin_args) = &segment.arguments else {
                return None;
            };
            let GenericArgument::Type(box_ty) = pin_args.args.first()? else {
                return None;
            };
            let Type::Path(box_path) = box_ty else {
                return None;
            };
            let box_seg = box_path.path.segments.last()?;
            if box_seg.ident != "Box" {
                return None;
            }
            let PathArguments::AngleBracketed(box_args) = &box_seg.arguments else {
                return None;
            };
            let GenericArgument::Type(dyn_ty) = box_args.args.first()? else {
                return None;
            };
            let Type::TraitObject(trait_obj) = dyn_ty else {
                return None;
            };
            for bound in &trait_obj.bounds {
                if let syn::TypeParamBound::Trait(trait_bound) = bound {
                    if let Some(item) = extract_item_from_stream_path(&trait_bound.path) {
                        return Some(item);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Given a path like `Stream<Item = T>` or `futures_core::Stream<Item = T>`,
/// extract the `T` from the `Item` associated type binding.
#[cfg(feature = "streaming")]
fn extract_item_from_stream_path(path: &syn::Path) -> Option<quote::__private::TokenStream> {
    let segment = path.segments.last()?;
    if segment.ident != "Stream" {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &segment.arguments else {
        return None;
    };
    for arg in &args.args {
        if let GenericArgument::AssocType(assoc) = arg {
            if assoc.ident == "Item" {
                let ty = &assoc.ty;
                return Some(quote! { #ty });
            }
        }
    }
    None
}

#[cfg(feature = "streaming")]
fn is_bus_argument(arg: &FnArg) -> bool {
    let FnArg::Typed(pat_type) = arg else {
        return false;
    };
    let Type::Reference(type_ref) = &*pat_type.ty else {
        return false;
    };
    let Type::Path(type_path) = &*type_ref.elem else {
        return false;
    };
    type_path
        .path
        .segments
        .last()
        .map(|segment| segment.ident == "Bus")
        .unwrap_or(false)
}

/// Derive macro for the `ResourceRequirement` marker trait.
///
/// Generates a blanket `impl ResourceRequirement for YourType {}`.
/// The type must also implement `Clone` (required by the Axon execution engine).
///
/// # Example
///
/// ```rust,ignore
/// use ranvier::prelude::*;
///
/// #[derive(Clone, ResourceRequirement)]
/// struct AppResources {
///     pool: sqlx::PgPool,
///     redis: redis::Client,
/// }
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

#[cfg(all(test, feature = "streaming"))]
mod tests {
    use super::is_bus_argument;
    use syn::{FnArg, parse_quote};

    #[test]
    fn detects_mut_bus_reference_argument() {
        let arg: FnArg = parse_quote!(bus: &mut Bus);
        assert!(is_bus_argument(&arg));
    }

    #[test]
    fn detects_fully_qualified_bus_reference_argument() {
        let arg: FnArg = parse_quote!(bus: &mut ranvier_core::bus::Bus);
        assert!(is_bus_argument(&arg));
    }

    #[test]
    fn rejects_non_bus_argument() {
        let arg: FnArg = parse_quote!(res: &MyResources);
        assert!(!is_bus_argument(&arg));
    }

    #[cfg(feature = "streaming")]
    mod streaming_tests {
        use crate::extract_result_stream_types;
        use syn::{Type, parse_quote};

        #[test]
        fn extracts_item_from_impl_stream() {
            let ty: Type = parse_quote!(Result<impl Stream<Item = ChatChunk> + Send, LlmError>);
            let (item, err) = extract_result_stream_types(&ty).expect("should parse stream types");
            assert_eq!(format!("{}", item), "ChatChunk");
            assert_eq!(format!("{}", err), "LlmError");
        }

        #[test]
        fn extracts_item_from_impl_stream_without_send() {
            let ty: Type = parse_quote!(Result<impl Stream<Item = String>, MyError>);
            let (item, err) = extract_result_stream_types(&ty).expect("should parse stream types");
            assert_eq!(format!("{}", item), "String");
            assert_eq!(format!("{}", err), "MyError");
        }

        #[test]
        fn extracts_item_from_pin_box_dyn_stream() {
            let ty: Type =
                parse_quote!(Result<Pin<Box<dyn Stream<Item = ChatChunk> + Send>>, String>);
            let (item, err) = extract_result_stream_types(&ty).expect("should parse stream types");
            assert_eq!(format!("{}", item), "ChatChunk");
            assert_eq!(format!("{}", err), "String");
        }

        #[test]
        fn returns_none_for_non_result_type() {
            let ty: Type = parse_quote!(Outcome<String, i32>);
            assert!(extract_result_stream_types(&ty).is_none());
        }
    }
}
