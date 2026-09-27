use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Error, GenericArgument, ItemFn, PathArguments, ReturnType, Type, TypeParamBound};

use crate::{
    config::TransitionArgs,
    core_crate_path,
    signature::{
        FunctionAttributes, ParameterContract, parse_function_attributes, parse_parameters,
        validate_function_signature,
    },
};

pub(crate) fn expand(attr: TokenStream2, item: TokenStream2) -> syn::Result<TokenStream2> {
    let function = syn::parse2::<ItemFn>(item)?;
    let args = TransitionArgs::parse(attr)?;
    args.validate_streaming_metadata()?;
    let function = StreamingFunction::parse(function, &args)?;
    let core_path = core_crate_path()?;

    let transition_type = format_ident!("__ranvier_stream_transition_{}", function.name);
    let name = &function.name;
    let visibility = &function.visibility;
    let shared_attributes = &function.attributes.shared;
    let documentation = &function.attributes.documentation;
    let input_type = &function.parameters.input_type;
    let resource_type = &function.parameters.resource_type;
    let bindings = &function.parameters.bindings;
    let item_type = &function.item_type;
    let error_type = &function.error_type;
    let body = &function.body;

    let run_body = if function.result_is_boxed {
        quote! { #body }
    } else {
        quote! {
            let __ranvier_stream_result = #body;
            __ranvier_stream_result.map(|stream| {
                ::std::boxed::Box::pin(stream)
                    as ::std::pin::Pin<::std::boxed::Box<
                        dyn #core_path::__macro_support::futures_core::Stream<Item = #item_type> + Send
                    >>
            })
        }
    };

    Ok(quote! {
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
        impl #core_path::streaming::StreamingTransition<#input_type> for #transition_type {
            type Item = #item_type;
            type Error = #error_type;
            type Resources = #resource_type;

            async fn run_stream(
                &self,
                input: #input_type,
                resources: &Self::Resources,
                bus: &mut #core_path::bus::Bus,
            ) -> ::std::result::Result<
                ::std::pin::Pin<::std::boxed::Box<
                    dyn #core_path::__macro_support::futures_core::Stream<Item = Self::Item> + Send
                >>,
                Self::Error,
            > {
                #bindings
                #run_body
            }
        }
    })
}

struct StreamingFunction {
    name: syn::Ident,
    visibility: syn::Visibility,
    attributes: FunctionAttributes,
    parameters: ParameterContract,
    item_type: Type,
    error_type: Type,
    result_is_boxed: bool,
    body: syn::Block,
}

impl StreamingFunction {
    fn parse(function: ItemFn, args: &TransitionArgs) -> syn::Result<Self> {
        validate_function_signature(&function)?;
        let parameters = parse_parameters(&function, args)?;
        let (item_type, error_type, result_is_boxed) = match &function.sig.output {
            ReturnType::Type(_, ty) => parse_result_stream_types(ty)?,
            ReturnType::Default => {
                return Err(Error::new_spanned(
                    &function.sig.ident,
                    "streaming transitions must declare Result<Stream, Error>",
                ));
            }
        };
        let attributes = parse_function_attributes(&function)?;

        Ok(Self {
            name: function.sig.ident,
            visibility: function.vis,
            attributes,
            parameters,
            item_type,
            error_type,
            result_is_boxed,
            body: *function.block,
        })
    }
}

fn parse_result_stream_types(ty: &Type) -> syn::Result<(Type, Type, bool)> {
    let Type::Path(type_path) = ty else {
        return Err(invalid_stream_return(ty));
    };
    if type_path.qself.is_some() {
        return Err(invalid_stream_return(ty));
    }
    let Some(result_segment) = type_path.path.segments.last() else {
        return Err(invalid_stream_return(ty));
    };
    if result_segment.ident != "Result" {
        return Err(Error::new_spanned(
            ty,
            "streaming transitions must return Result<Stream, Error>",
        ));
    }
    let PathArguments::AngleBracketed(result_arguments) = &result_segment.arguments else {
        return Err(invalid_stream_return(ty));
    };
    if result_arguments.args.len() != 2 {
        return Err(invalid_stream_return(ty));
    }
    let mut arguments = result_arguments.args.iter();
    let (Some(GenericArgument::Type(stream_type)), Some(GenericArgument::Type(error_type))) =
        (arguments.next(), arguments.next())
    else {
        return Err(invalid_stream_return(ty));
    };

    let (item_type, result_is_boxed) = parse_stream_type(stream_type)?;
    Ok((item_type, error_type.clone(), result_is_boxed))
}

fn parse_stream_type(ty: &Type) -> syn::Result<(Type, bool)> {
    match ty {
        Type::ImplTrait(impl_trait) => Ok((parse_stream_bounds(&impl_trait.bounds, ty)?, false)),
        Type::Path(pin_path) => {
            let Some(pin_segment) = pin_path.path.segments.last() else {
                return Err(invalid_stream_return(ty));
            };
            if pin_path.qself.is_some() || pin_segment.ident != "Pin" {
                return Err(invalid_stream_return(ty));
            }
            let PathArguments::AngleBracketed(pin_arguments) = &pin_segment.arguments else {
                return Err(invalid_stream_return(ty));
            };
            if pin_arguments.args.len() != 1 {
                return Err(invalid_stream_return(ty));
            }
            let Some(GenericArgument::Type(box_type)) = pin_arguments.args.first() else {
                return Err(invalid_stream_return(ty));
            };
            let Type::Path(box_path) = box_type else {
                return Err(invalid_stream_return(ty));
            };
            let Some(box_segment) = box_path.path.segments.last() else {
                return Err(invalid_stream_return(ty));
            };
            if box_path.qself.is_some() || box_segment.ident != "Box" {
                return Err(invalid_stream_return(ty));
            }
            let PathArguments::AngleBracketed(box_arguments) = &box_segment.arguments else {
                return Err(invalid_stream_return(ty));
            };
            if box_arguments.args.len() != 1 {
                return Err(invalid_stream_return(ty));
            }
            let Some(GenericArgument::Type(Type::TraitObject(trait_object))) =
                box_arguments.args.first()
            else {
                return Err(invalid_stream_return(ty));
            };
            Ok((parse_stream_bounds(&trait_object.bounds, ty)?, true))
        }
        _ => Err(invalid_stream_return(ty)),
    }
}

fn parse_stream_bounds(
    bounds: &syn::punctuated::Punctuated<TypeParamBound, syn::Token![+]>,
    authored_type: &Type,
) -> syn::Result<Type> {
    let mut item_type = None;
    let mut has_send = false;
    let mut has_stream = false;

    for bound in bounds {
        let TypeParamBound::Trait(trait_bound) = bound else {
            return Err(invalid_stream_return(authored_type));
        };
        if trait_bound.lifetimes.is_some()
            || !matches!(&trait_bound.modifier, &syn::TraitBoundModifier::None)
        {
            return Err(invalid_stream_return(authored_type));
        }
        let Some(segment) = trait_bound.path.segments.last() else {
            return Err(invalid_stream_return(authored_type));
        };
        if segment.ident == "Send" {
            if has_send
                || trait_bound.path.leading_colon.is_some()
                || trait_bound.path.segments.len() != 1
                || !matches!(segment.arguments, PathArguments::None)
            {
                return Err(invalid_stream_return(authored_type));
            }
            has_send = true;
        } else if segment.ident == "Stream" {
            if has_stream {
                return Err(invalid_stream_return(authored_type));
            }
            has_stream = true;
            let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
                return Err(Error::new_spanned(
                    authored_type,
                    "streaming transition return stream must declare exactly one `Item = Type` binding",
                ));
            };
            if arguments.args.len() != 1 {
                return Err(Error::new_spanned(
                    authored_type,
                    "streaming transition return stream must declare exactly one `Item = Type` binding",
                ));
            }
            let Some(GenericArgument::AssocType(item_binding)) = arguments.args.first() else {
                return Err(Error::new_spanned(
                    authored_type,
                    "streaming transition return stream must declare exactly one `Item = Type` binding",
                ));
            };
            if item_binding.ident != "Item" {
                return Err(Error::new_spanned(
                    authored_type,
                    "streaming transition return stream must declare exactly one `Item = Type` binding",
                ));
            }
            item_type = Some(item_binding.ty.clone());
        } else {
            return Err(invalid_stream_return(authored_type));
        }
    }

    let Some(item_type) = item_type else {
        return Err(Error::new_spanned(
            authored_type,
            "streaming transition return stream must declare exactly one `Item = Type` binding",
        ));
    };
    if !has_send {
        return Err(Error::new_spanned(
            authored_type,
            "streaming transition return stream must include `Send`",
        ));
    }
    if !has_stream {
        return Err(invalid_stream_return(authored_type));
    }
    Ok(item_type)
}

fn invalid_stream_return(ty: &Type) -> Error {
    Error::new_spanned(
        ty,
        "streaming transitions support Result<impl Stream<Item = Item> + Send, Error> or Result<Pin<Box<dyn Stream<Item = Item> + Send>>, Error>",
    )
}

#[cfg(test)]
mod tests {
    use super::parse_result_stream_types;
    use syn::{Type, parse_quote};

    #[test]
    fn parses_impl_stream_return() {
        let ty: Type = parse_quote!(Result<impl Stream<Item = ChatChunk> + Send, LlmError>);
        let (item, error, boxed) = parse_result_stream_types(&ty).expect("valid stream return");
        assert_eq!(item.to_token_stream_string(), "ChatChunk");
        assert_eq!(error.to_token_stream_string(), "LlmError");
        assert!(!boxed);
    }

    #[test]
    fn parses_qualified_pinned_boxed_stream_return() {
        let ty: Type = parse_quote!(
            std::result::Result<
                std::pin::Pin<std::boxed::Box<dyn futures_core::Stream<Item = ChatChunk> + Send>>,
                LlmError,
            >
        );
        let (item, error, boxed) = parse_result_stream_types(&ty).expect("valid stream return");
        assert_eq!(item.to_token_stream_string(), "ChatChunk");
        assert_eq!(error.to_token_stream_string(), "LlmError");
        assert!(boxed);
    }

    #[test]
    fn rejects_stream_without_send() {
        let ty: Type = parse_quote!(Result<impl Stream<Item = String>, MyError>);
        let error = parse_result_stream_types(&ty).expect_err("Send is part of the contract");
        assert!(error.to_string().contains("must include `Send`"));
    }

    #[test]
    fn rejects_optional_send_bound() {
        let ty: Type = syn::parse_str("Result<impl Stream<Item = String> + ?Send, MyError>")
            .expect("optional trait bound is syntactically valid");
        let error = parse_result_stream_types(&ty).expect_err("?Send does not guarantee Send");
        assert!(error.to_string().contains("support Result<impl Stream"));
    }

    #[test]
    fn rejects_noncanonical_send_path() {
        let ty: Type = parse_quote!(Result<impl Stream<Item = String> + custom::Send, MyError>);
        let error = parse_result_stream_types(&ty).expect_err("only the Send marker is accepted");
        assert!(error.to_string().contains("support Result<impl Stream"));
    }

    #[test]
    fn rejects_higher_ranked_stream_bound() {
        let ty: Type = syn::parse_str("Result<impl for<'a> Stream<Item = String> + Send, MyError>")
            .expect("higher-ranked stream bound is syntactically valid");
        let error =
            parse_result_stream_types(&ty).expect_err("higher-ranked bounds are unsupported");
        assert!(error.to_string().contains("support Result<impl Stream"));
    }

    #[test]
    fn rejects_stream_without_item() {
        let ty: Type = parse_quote!(Result<impl Stream + Send, MyError>);
        let error = parse_result_stream_types(&ty).expect_err("Item is required");
        assert!(error.to_string().contains("exactly one `Item = Type`"));
    }

    trait TypeToString {
        fn to_token_stream_string(&self) -> String;
    }

    impl TypeToString for Type {
        fn to_token_stream_string(&self) -> String {
            quote::ToTokens::to_token_stream(self).to_string()
        }
    }
}
