use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;

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
