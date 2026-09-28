use proc_macro2::TokenStream as TokenStream2;
use syn::{Attribute, Ident, Visibility};

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

pub(crate) struct ParameterContract {
    pub(crate) input_type: TokenStream2,
    pub(crate) resource_type: TokenStream2,
    pub(crate) bindings: TokenStream2,
}

pub(crate) struct FunctionAttributes {
    pub(crate) shared: Vec<Attribute>,
    pub(crate) documentation: Vec<Attribute>,
}
