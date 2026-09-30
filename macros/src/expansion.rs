use crate::{config::TransitionArgs, model::TransitionFunction};
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::ItemFn;

pub(crate) fn expand_transition(
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
