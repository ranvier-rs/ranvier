use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};

#[proc_macro_attribute]
pub fn module(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let name = &input.sig.ident;
    let struct_name = syn::Ident::new(&format!("{}Module", name), name.span());
    let block = &input.block;
    let label = name.to_string();

    // Naive implementation:
    // 1. Create a struct named {FunctionName}Module
    // 2. Implement Module trait for it.
    // 3. Move function body into execute method.

    let expanded = quote! {
        pub struct #struct_name;

        #[async_trait::async_trait]
        impl ranvier_core::Module for #struct_name {
            fn metadata(&self) -> ranvier_core::StepMetadata {
                ranvier_core::StepMetadata {
                    id: uuid::Uuid::new_v4(), // Warning: This generates new ID every call. In real implementation, this should be static or deterministic.
                    label: #label.to_string(),
                    description: None,
                    inputs: vec![],
                    outputs: vec![],
                }
            }

            async fn execute(&self, _bus: &mut ranvier_core::Bus) -> ranvier_core::ModuleResult {
                // Wrapper to call the original logic
                // For MVP, we presume the body interacts with nothing or just prints.
                // To properly support args, we need complex parsing.
                // Let's just inline the body for the MVP "function-like" experience.

                async fn inner_logic() {
                    #block
                }

                inner_logic().await;

                Ok(())
            }
        }

        // Expose the original function name as a constructor or instance
        #[allow(non_upper_case_globals)]
        pub const #name: #struct_name = #struct_name;
    };

    TokenStream::from(expanded)
}
