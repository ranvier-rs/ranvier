use proc_macro::TokenStream;
use quote::quote;
use syn::{FnArg, GenericArgument, ItemFn, PathArguments, ReturnType, Type, parse_macro_input};

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

    // 2. Extract Resources Type
    let res_type = if let Some(res) = res_override {
        quote! { #res }
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
            if let Some(FnArg::Typed(pat_type)) = inputs.get(1) {
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

    let expanded = quote! {
        #[derive(Clone, Default)]
        #[allow(non_camel_case_types)]
        #vis struct #original_ident;

        #[async_trait::async_trait]
        impl ranvier_core::transition::Transition<#input_type, #output_type> for #original_ident {
            type Error = #error_type;
            type Resources = #res_type;

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

fn extract_outcome_types(
    ty: &Type,
) -> Option<(quote::__private::TokenStream, quote::__private::TokenStream)> {
    if let Type::Path(type_path) = ty {
        if let Some(segment) = type_path.path.segments.last() {
            if segment.ident == "Outcome" {
                if let PathArguments::AngleBracketed(args) = &segment.arguments {
                    let mut type_args = args.args.iter();
                    if let (Some(GenericArgument::Type(to)), Some(GenericArgument::Type(err))) =
                        (type_args.next(), type_args.next())
                    {
                        return Some((quote! { #to }, quote! { #err }));
                    }
                }
            }
        }
    }
    None
}
