use proc_macro::TokenStream;

pub(crate) fn compile_error(error: syn::Error) -> TokenStream {
    error.into_compile_error().into()
}
