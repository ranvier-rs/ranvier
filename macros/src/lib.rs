use proc_macro::TokenStream;
use quote::{ToTokens, quote};
use std::collections::HashSet;
use syn::{
    DeriveInput, FnArg, GenericArgument, ItemFn, PathArguments, ReturnType, Type,
    parse_macro_input,
};

/// Attribute macro to transform an async function into a `Transition` implementation.
#[proc_macro_attribute]
pub fn transition(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut input_fn = parse_macro_input!(item as ItemFn);
    let original_ident = input_fn.sig.ident.clone();
    let vis = &input_fn.vis;
    let block = &input_fn.block;
    let inputs = &input_fn.sig.inputs;

    // We don't rename the function here, instead we use a prefix for the struct.
    // However, to make .then(multiply_by_res) work, multiply_by_res MUST be the struct name.
    // So we rename the FUNCTION and keep the name for the STRUCT.
    let internal_fn_ident = quote::format_ident!("__ranvier_fn_{}", original_ident);
    input_fn.sig.ident = internal_fn_ident.clone();

    // Parse attribute for explicit resource type override
    let mut res_override = None;
    let mut bus_allow_types: Vec<Type> = Vec::new();
    let mut bus_deny_types: Vec<Type> = Vec::new();
    let mut bus_allow_specified = false;
    let mut bus_deny_specified = false;
    let mut x_pos = None;
    let mut y_pos = None;
    let mut schema_flag = false;
    if !attr.is_empty() {
        let parser = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated;
        if let Ok(metas) = syn::parse::Parser::parse2(parser, attr.into()) {
            for meta in metas {
                match meta {
                    syn::Meta::Path(path) if path.is_ident("schema") => {
                        schema_flag = true;
                    }
                    syn::Meta::NameValue(nv) => {
                        if nv.path.is_ident("res") {
                            res_override = Some(nv.value);
                        } else if nv.path.is_ident("bus_allow") {
                            bus_allow_specified = true;
                            match parse_type_array_expr(&nv.value) {
                                Ok(types) => bus_allow_types = types,
                                Err(err) => return err.to_compile_error().into(),
                            }
                        } else if nv.path.is_ident("bus_deny") {
                            bus_deny_specified = true;
                            match parse_type_array_expr(&nv.value) {
                                Ok(types) => bus_deny_types = types,
                                Err(err) => return err.to_compile_error().into(),
                            }
                        } else if nv.path.is_ident("x") {
                            x_pos = Some(nv.value);
                        } else if nv.path.is_ident("y") {
                            y_pos = Some(nv.value);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if let Err(err) = validate_bus_policy_types(&bus_allow_types, &bus_deny_types) {
        return err.to_compile_error().into();
    }

    // 1. Extract Input Type (From)
    let input_type = if let Some(FnArg::Typed(pat_type)) = inputs.first() {
        let ty = &pat_type.ty;
        quote! { #ty }
    } else {
        quote! { () }
    };

    // 2. Extract Resources Type
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

    // 3. Extract Outcome Types
    let (output_type, error_type) = if let ReturnType::Type(_, ty) = &input_fn.sig.output {
        extract_outcome_types(ty).unwrap_or((quote! { () }, quote! { anyhow::Error }))
    } else {
        (quote! { () }, quote! { anyhow::Error })
    };

    // 4. Handle Arguments
    let arg_count = inputs.len();
    let run_body = match arg_count {
        1 => {
            if let Some(FnArg::Typed(pat_type)) = inputs.first() {
                let pat = &pat_type.pat;
                quote! { let #pat = input; #block }
            } else {
                quote! { #block }
            }
        }
        2 => {
            let mut bindings = quote! {};
            if let Some(FnArg::Typed(pat_type)) = inputs.first() {
                let pat = &pat_type.pat;
                bindings.extend(quote! { let #pat = input; });
            }
            if second_is_bus {
                if let Some(FnArg::Typed(pat_type)) = inputs.get(1) {
                    let pat = &pat_type.pat;
                    bindings.extend(quote! { let #pat = bus; });
                }
            } else if let Some(FnArg::Typed(pat_type)) = inputs.get(1) {
                let pat = &pat_type.pat;
                bindings.extend(quote! { let #pat = resources; });
            }
            quote! { #bindings #block }
        }
        3 => {
            let mut bindings = quote! {};
            if let Some(FnArg::Typed(pat_type)) = inputs.first() {
                let pat = &pat_type.pat;
                bindings.extend(quote! { let #pat = input; });
            }
            if let Some(FnArg::Typed(pat_type)) = inputs.get(1) {
                let pat = &pat_type.pat;
                bindings.extend(quote! { let #pat = resources; });
            }
            if let Some(FnArg::Typed(pat_type)) = inputs.get(2) {
                let pat = &pat_type.pat;
                bindings.extend(quote! { let #pat = bus; });
            }
            quote! { #bindings #block }
        }
        _ => quote! { #block },
    };

    let bus_policy_method = if bus_allow_specified || bus_deny_specified {
        let allow_expr = if bus_allow_specified {
            quote! {
                Some(vec![#(ranvier_core::bus::BusTypeRef::of::<#bus_allow_types>()),*])
            }
        } else {
            quote! { None }
        };
        let deny_expr = if bus_deny_specified {
            quote! {
                vec![#(ranvier_core::bus::BusTypeRef::of::<#bus_deny_types>()),*]
            }
        } else {
            quote! { Vec::new() }
        };
        quote! {
            fn bus_access_policy(&self) -> Option<ranvier_core::bus::BusAccessPolicy> {
                Some(ranvier_core::bus::BusAccessPolicy {
                    allow: #allow_expr,
                    deny: #deny_expr,
                })
            }
        }
    } else {
        quote! {}
    };

    let position_method = if let (Some(x), Some(y)) = (x_pos, y_pos) {
        quote! {
            fn position(&self) -> Option<(f32, f32)> {
                Some((#x as f32, #y as f32))
            }
        }
    } else {
        quote! {}
    };

    let schema_method = if schema_flag {
        quote! {
            fn input_schema(&self) -> Option<serde_json::Value> {
                let schema = schemars::schema_for!(#input_type);
                serde_json::to_value(schema).ok()
            }
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        #[derive(Clone, Default)]
        #[allow(non_camel_case_types)]
        #vis struct #original_ident;

        #[::async_trait::async_trait]
        impl ranvier_core::transition::Transition<#input_type, #output_type> for #original_ident {
            type Error = #error_type;
            type Resources = #res_type;

            #bus_policy_method
            #position_method
            #schema_method

            async fn run(
                &self,
                input: #input_type,
                resources: &Self::Resources,
                bus: &mut ranvier_core::bus::Bus,
            ) -> ranvier_core::outcome::Outcome<#output_type, Self::Error> {
                #run_body
            }
        }

        #input_fn
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
/// - `res = MyType` — explicit resource type override
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

    // Parse attribute for explicit resource type override
    let mut res_override = None;
    if !attr.is_empty() {
        let parser = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated;
        if let Ok(metas) = syn::parse::Parser::parse2(parser, attr.into()) {
            for meta in metas {
                if let syn::Meta::NameValue(nv) = meta {
                    if nv.path.is_ident("res") {
                        res_override = Some(nv.value);
                    }
                }
            }
        }
    }

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

fn extract_outcome_types(
    ty: &Type,
) -> Option<(quote::__private::TokenStream, quote::__private::TokenStream)> {
    if let Type::Path(type_path) = ty
        && let Some(segment) = type_path.path.segments.last()
        && segment.ident == "Outcome"
        && let PathArguments::AngleBracketed(args) = &segment.arguments
    {
        let mut type_args = args.args.iter();
        if let (Some(GenericArgument::Type(to)), Some(GenericArgument::Type(err))) =
            (type_args.next(), type_args.next())
        {
            return Some((quote! { #to }, quote! { #err }));
        }
    }
    None
}

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

fn parse_type_array_expr(expr: &syn::Expr) -> syn::Result<Vec<Type>> {
    let syn::Expr::Array(array) = expr else {
        return Err(syn::Error::new_spanned(
            expr,
            "expected array syntax: [TypeA, TypeB]",
        ));
    };

    array
        .elems
        .iter()
        .map(|elem| syn::parse2::<Type>(elem.to_token_stream()))
        .collect()
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
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let expanded = quote! {
        impl #impl_generics ranvier_core::transition::ResourceRequirement for #name #ty_generics #where_clause {}
    };

    TokenStream::from(expanded)
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
    use super::{is_bus_argument, parse_type_array_expr, validate_bus_policy_types};
    use syn::{Expr, FnArg, parse_quote};

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

    #[test]
    fn parses_type_array_expr_for_bus_policy() {
        let expr: Expr = parse_quote!([i32, alloc::string::String]);
        let parsed = parse_type_array_expr(&expr).expect("type array should parse");
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn validates_bus_policy_rejects_duplicate_allow() {
        let allow = vec![parse_quote!(i32), parse_quote!(i32)];
        let deny = Vec::new();
        let err = validate_bus_policy_types(&allow, &deny).expect_err("should fail");
        assert!(err.to_string().contains("duplicate type in bus_allow"));
    }

    #[test]
    fn validates_bus_policy_rejects_allow_deny_conflict() {
        let allow = vec![parse_quote!(i32)];
        let deny = vec![parse_quote!(i32)];
        let err = validate_bus_policy_types(&allow, &deny).expect_err("should fail");
        assert!(
            err.to_string()
                .contains("same type cannot be present in both bus_allow and bus_deny")
        );
    }

    #[cfg(feature = "streaming")]
    mod streaming_tests {
        use crate::{extract_result_stream_types};
        use syn::{Type, parse_quote};

        #[test]
        fn extracts_item_from_impl_stream() {
            let ty: Type = parse_quote!(Result<impl Stream<Item = ChatChunk> + Send, LlmError>);
            let (item, err) =
                extract_result_stream_types(&ty).expect("should parse stream types");
            assert_eq!(format!("{}", item), "ChatChunk");
            assert_eq!(format!("{}", err), "LlmError");
        }

        #[test]
        fn extracts_item_from_impl_stream_without_send() {
            let ty: Type = parse_quote!(Result<impl Stream<Item = String>, MyError>);
            let (item, err) =
                extract_result_stream_types(&ty).expect("should parse stream types");
            assert_eq!(format!("{}", item), "String");
            assert_eq!(format!("{}", err), "MyError");
        }

        #[test]
        fn extracts_item_from_pin_box_dyn_stream() {
            let ty: Type =
                parse_quote!(Result<Pin<Box<dyn Stream<Item = ChatChunk> + Send>>, String>);
            let (item, err) =
                extract_result_stream_types(&ty).expect("should parse stream types");
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
