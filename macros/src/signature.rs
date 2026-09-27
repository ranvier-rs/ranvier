use crate::config::TransitionArgs;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Attribute, Error, FnArg, Ident, ItemFn, PatType, ReturnType, Type, Visibility};

pub(crate) struct TransitionFunction {
    pub(crate) name: Ident,
    pub(crate) visibility: Visibility,
    pub(crate) shared_attributes: Vec<Attribute>,
    pub(crate) documentation: Vec<Attribute>,
    pub(crate) input_type: TokenStream2,
    pub(crate) resource_type: TokenStream2,
    pub(crate) bindings: TokenStream2,
    pub(crate) output_type: TokenStream2,
    pub(crate) error_type: TokenStream2,
    pub(crate) body: syn::Block,
}

enum ParameterRole {
    Input(Type),
    Resources(Type),
    Bus,
}

impl ParameterRole {
    fn rank(&self) -> u8 {
        match self {
            Self::Input(_) => 0,
            Self::Resources(_) => 1,
            Self::Bus => 2,
        }
    }
}

pub(crate) struct ParameterContract {
    pub(crate) input_type: TokenStream2,
    pub(crate) resource_type: TokenStream2,
    pub(crate) bindings: TokenStream2,
}

pub(crate) struct FunctionAttributes {
    pub(crate) shared: Vec<Attribute>,
    pub(crate) documentation: Vec<Attribute>,
}

pub(crate) fn validate_function_signature(function: &ItemFn) -> syn::Result<()> {
    let signature = &function.sig;
    if signature.asyncness.is_none() {
        return Err(Error::new_spanned(
            &signature.ident,
            "transition functions must be async",
        ));
    }
    if let Some(token) = &signature.constness {
        return Err(Error::new_spanned(
            token,
            "transition functions cannot be const",
        ));
    }
    if let Some(token) = &signature.unsafety {
        return Err(Error::new_spanned(
            token,
            "transition functions cannot be unsafe",
        ));
    }
    if let Some(abi) = &signature.abi {
        return Err(Error::new_spanned(
            abi,
            "transition functions cannot use an extern ABI",
        ));
    }
    if let Some(variadic) = &signature.variadic {
        return Err(Error::new_spanned(
            variadic,
            "transition functions cannot be variadic",
        ));
    }
    if !signature.generics.params.is_empty() || signature.generics.where_clause.is_some() {
        return Err(Error::new_spanned(
            &signature.generics,
            "transition functions cannot declare generics or a where clause",
        ));
    }
    Ok(())
}

pub(crate) fn parse_parameters(
    function: &ItemFn,
    args: &TransitionArgs,
) -> syn::Result<ParameterContract> {
    let mut input_type = quote!(());
    let mut resource_type = args
        .resources
        .as_ref()
        .map(|resources| quote!(#resources))
        .unwrap_or_else(|| quote!(()));
    let mut bindings = Vec::new();
    let mut input_seen = false;
    let mut resources_seen = false;
    let mut bus_seen = false;
    let mut previous_rank = None;

    for argument in &function.sig.inputs {
        let FnArg::Typed(PatType { pat, ty, .. }) = argument else {
            return Err(Error::new_spanned(
                argument,
                "transition functions cannot have a receiver",
            ));
        };
        let role = classify_parameter(ty)?;
        let rank = role.rank();

        match &role {
            ParameterRole::Input(_) if input_seen => {
                return Err(Error::new_spanned(
                    argument,
                    "transition functions accept at most one input parameter",
                ));
            }
            ParameterRole::Resources(_) if resources_seen => {
                return Err(Error::new_spanned(
                    argument,
                    "transition functions accept at most one resources parameter",
                ));
            }
            ParameterRole::Bus if bus_seen => {
                return Err(Error::new_spanned(
                    argument,
                    "transition functions accept at most one Bus parameter",
                ));
            }
            _ => {}
        }

        if previous_rank.is_some_and(|previous| rank < previous) {
            return Err(Error::new_spanned(
                argument,
                "transition parameters must follow input, resources, Bus role order",
            ));
        }
        previous_rank = Some(rank);

        match role {
            ParameterRole::Input(ty) => {
                input_seen = true;
                input_type = quote!(#ty);
                bindings.push(quote!(let #pat = input;));
            }
            ParameterRole::Resources(ty) => {
                resources_seen = true;
                if let Some(span) = args.resources_span {
                    return Err(Error::new(
                        span,
                        "resources = Type cannot be combined with a resources parameter",
                    ));
                }
                resource_type = quote!(#ty);
                bindings.push(quote!(let #pat = resources;));
            }
            ParameterRole::Bus => {
                bus_seen = true;
                bindings.push(quote!(let #pat = bus;));
            }
        }
    }

    Ok(ParameterContract {
        input_type,
        resource_type,
        bindings: quote!(#(#bindings)*),
    })
}

pub(crate) fn parse_function_attributes(function: &ItemFn) -> syn::Result<FunctionAttributes> {
    let mut shared = Vec::new();
    let mut documentation = Vec::new();
    for attribute in &function.attrs {
        let path = attribute.path();
        if path.is_ident("doc") {
            documentation.push(attribute.clone());
        } else if path.is_ident("cfg")
            || path.is_ident("cfg_attr")
            || path.is_ident("allow")
            || path.is_ident("warn")
            || path.is_ident("deny")
            || path.is_ident("forbid")
            || path.is_ident("expect")
        {
            shared.push(attribute.clone());
        } else {
            return Err(Error::new_spanned(
                attribute,
                format!(
                    "function attribute {} is not supported on a transition",
                    quote!(#path)
                ),
            ));
        }
    }
    Ok(FunctionAttributes {
        shared,
        documentation,
    })
}

impl TransitionFunction {
    pub(crate) fn parse(function: ItemFn, args: &TransitionArgs) -> syn::Result<Self> {
        validate_function_signature(&function)?;
        let parameters = parse_parameters(&function, args)?;
        let signature = &function.sig;

        let (output_type, error_type) = match &signature.output {
            ReturnType::Type(_, ty) => parse_outcome_types(ty)?,
            ReturnType::Default => {
                return Err(Error::new_spanned(
                    &signature.ident,
                    "transition functions must declare Outcome<Output, Error>",
                ));
            }
        };

        let attributes = parse_function_attributes(&function)?;

        Ok(Self {
            name: signature.ident.clone(),
            visibility: function.vis,
            shared_attributes: attributes.shared,
            documentation: attributes.documentation,
            input_type: parameters.input_type,
            resource_type: parameters.resource_type,
            bindings: parameters.bindings,
            output_type,
            error_type,
            body: *function.block,
        })
    }
}

fn classify_parameter(ty: &Type) -> syn::Result<ParameterRole> {
    match ty {
        Type::Reference(reference) if is_bus_type(&reference.elem) => {
            if reference.mutability.is_some() {
                Ok(ParameterRole::Bus)
            } else {
                Err(Error::new_spanned(
                    ty,
                    "Bus parameters must use a mutable reference: &mut Bus",
                ))
            }
        }
        Type::Reference(reference) if reference.mutability.is_some() => Err(Error::new_spanned(
            ty,
            "resource parameters must use a shared reference; mutable non-Bus references are unsupported",
        )),
        Type::Reference(reference) => Ok(ParameterRole::Resources((*reference.elem).clone())),
        _ if is_bus_type(ty) => Err(Error::new_spanned(
            ty,
            "Bus parameters must use a mutable reference: &mut Bus",
        )),
        _ => Ok(ParameterRole::Input(ty.clone())),
    }
}

fn is_bus_type(ty: &Type) -> bool {
    match ty {
        Type::Path(path) => {
            path.qself.is_none()
                && path
                    .path
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == "Bus")
        }
        Type::Paren(parenthesized) => is_bus_type(&parenthesized.elem),
        Type::Group(group) => is_bus_type(&group.elem),
        _ => false,
    }
}

fn parse_outcome_types(ty: &Type) -> syn::Result<(TokenStream2, TokenStream2)> {
    let Type::Path(type_path) = ty else {
        return Err(invalid_outcome_type(ty));
    };
    if type_path.qself.is_some() {
        return Err(invalid_outcome_type(ty));
    }
    let Some(segment) = type_path.path.segments.last() else {
        return Err(invalid_outcome_type(ty));
    };
    if segment.ident != "Outcome" {
        return Err(invalid_outcome_type(ty));
    }
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(invalid_outcome_type(ty));
    };
    if arguments.args.len() != 2 {
        return Err(invalid_outcome_type(ty));
    }
    let mut arguments = arguments.args.iter();
    let (Some(syn::GenericArgument::Type(output)), Some(syn::GenericArgument::Type(error))) =
        (arguments.next(), arguments.next())
    else {
        return Err(invalid_outcome_type(ty));
    };
    Ok((quote!(#output), quote!(#error)))
}

fn invalid_outcome_type(ty: &Type) -> Error {
    Error::new_spanned(
        ty,
        "transition functions must return Outcome<Output, Error> with exactly two type arguments",
    )
}
