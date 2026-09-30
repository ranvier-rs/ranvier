use proc_macro::TokenStream;
mod config;
mod crate_path;
mod diagnostic;
mod expansion;
mod model;
mod signature;
#[cfg(feature = "streaming")]
mod streaming;

/// Attribute macro to transform an async function into a `Transition` implementation.
#[proc_macro_attribute]
pub fn transition(attr: TokenStream, item: TokenStream) -> TokenStream {
    crate_path::core_crate_path()
        .and_then(|core_path| expansion::expand_transition(core_path, attr.into(), item.into()))
        .map(TokenStream::from)
        .unwrap_or_else(diagnostic::compile_error)
}

/// Attribute macro to transform an async function into a `StreamingTransition` implementation.
///
/// The function must return `Result<impl Stream<Item = T> + Send, E>`.
/// The macro generates a zero-sized struct and `StreamingTransition` trait impl.
///
/// ## Parameter Binding
///
/// Follows the same ordered roles as `#[transition]`, with every role optional:
/// - no params: `()`
/// - 1 param: `(input)`, `(&Resources)`, or `(&mut Bus)`
/// - 2 params: `(input, &Resources)`, `(input, &mut Bus)`, or `(&Resources, &mut Bus)`
/// - 3 params: `(input, &Resources, &mut Bus)`
///
/// A shared non-Bus reference is resources, not a borrowed input. Its referent
/// must satisfy `Send + Sync + 'static`. Bus must be a mutable reference.
///
/// ## Attributes
///
/// - `resources = MyType` — declares the resource type only when there is no
///   resource parameter. Combining this attribute with `&Resources` is rejected.
///
/// # Example
///
/// ```rust
/// use std::{pin::Pin, task::{Context, Poll}};
/// use ranvier_core::{
///     __macro_support::futures_core::Stream,
///     bus::Bus,
/// };
/// use ranvier_macros::streaming_transition;
///
/// #[derive(Clone)]
/// struct AppResources;
///
/// struct OneItemStream(Option<String>);
///
/// impl Stream for OneItemStream {
///     type Item = String;
///
///     fn poll_next(
///         mut self: Pin<&mut Self>,
///         _context: &mut Context<'_>,
///     ) -> Poll<Option<Self::Item>> {
///         Poll::Ready(self.0.take())
///     }
/// }
///
/// #[streaming_transition]
/// async fn synthesize(
///     input: String,
///     _resources: &AppResources,
///     _bus: &mut Bus,
/// ) -> Result<impl Stream<Item = String> + Send, String> {
///     Ok(OneItemStream(Some(input)))
/// }
///
/// fn assert_streaming_transition<T>(_: &T)
/// where
///     T: ranvier_core::streaming::StreamingTransition<
///         String,
///         Item = String,
///         Error = String,
///         Resources = AppResources,
///     > + Clone,
/// {
/// }
///
/// assert_streaming_transition(&synthesize);
/// ```
#[cfg(feature = "streaming")]
#[proc_macro_attribute]
pub fn streaming_transition(attr: TokenStream, item: TokenStream) -> TokenStream {
    match streaming::expand(attr.into(), item.into()) {
        Ok(expansion) => expansion.into(),
        Err(error) => diagnostic::compile_error(error),
    }
}

#[cfg(test)]
mod ordinary_expansion_tests {
    use super::expansion::expand_transition;
    use proc_macro2::TokenStream as TokenStream2;
    use quote::{ToTokens, quote};
    use syn::{Attribute, Item, parse_quote};

    fn attribute_count(attributes: &[Attribute], expected: &Attribute) -> usize {
        let expected_tokens = expected.to_token_stream().to_string();
        attributes
            .iter()
            .filter(|attribute| attribute.to_token_stream().to_string() == expected_tokens)
            .count()
    }

    fn documentation_values(attributes: &[Attribute]) -> Vec<String> {
        attributes
            .iter()
            .filter_map(|attribute| {
                let syn::Meta::NameValue(documentation) = &attribute.meta else {
                    return None;
                };
                if !documentation.path.is_ident("doc") {
                    return None;
                }
                let syn::Expr::Lit(documentation) = &documentation.value else {
                    return None;
                };
                let syn::Lit::Str(documentation) = &documentation.lit else {
                    return None;
                };
                Some(documentation.value())
            })
            .collect()
    }

    #[test]
    fn copies_shared_attributes_to_each_generated_item() {
        let source = quote! {
            #[cfg(any(feature = "ordinary_attr_a", feature = "ordinary_attr_b"))]
            #[cfg_attr(all(), doc = "conditional ordinary transition documentation")]
            #[allow(dead_code)]
            /// Ordinary transition documentation.
            async fn attributed(
                input: i32,
            ) -> ranvier_core::outcome::Outcome<i32, std::convert::Infallible> {
                ranvier_core::outcome::Outcome::next(input)
            }
        };
        let expanded = expand_transition(quote!(::ranvier_core), TokenStream2::new(), source)
            .expect("valid ordinary transition function");
        let file = syn::parse2::<syn::File>(expanded).expect("generated ordinary items parse");
        assert_eq!(
            file.items.len(),
            3,
            "type, value, and trait impl are generated"
        );

        let [
            Item::Struct(transition_type),
            Item::Const(transition_value),
            Item::Impl(transition_impl),
        ] = file.items.as_slice()
        else {
            panic!("expected generated struct, const, and impl in order");
        };

        let expected_shared: [Attribute; 3] = [
            parse_quote!(#[cfg(any(feature = "ordinary_attr_a", feature = "ordinary_attr_b"))]),
            parse_quote!(#[cfg_attr(all(), doc = "conditional ordinary transition documentation")]),
            parse_quote!(#[allow(dead_code)]),
        ];
        let generated_attributes = [
            ("transition type", transition_type.attrs.as_slice()),
            ("transition value", transition_value.attrs.as_slice()),
            ("transition impl", transition_impl.attrs.as_slice()),
        ];

        for (generated_name, attributes) in generated_attributes {
            for expected_attribute in &expected_shared {
                assert_eq!(
                    attribute_count(attributes, expected_attribute),
                    1,
                    "{generated_name} must contain exactly one {:?} attribute",
                    expected_attribute.path(),
                );
            }
        }

        let source_documentation = " Ordinary transition documentation.";
        assert_eq!(
            documentation_values(&transition_type.attrs)
                .iter()
                .filter(|value| value.as_str() == source_documentation)
                .count(),
            0,
            "hidden type does not receive source docs",
        );
        assert_eq!(
            documentation_values(&transition_value.attrs)
                .iter()
                .filter(|value| value.as_str() == source_documentation)
                .count(),
            1,
            "named value receives the source doc exactly once",
        );
        assert_eq!(
            documentation_values(&transition_impl.attrs)
                .iter()
                .filter(|value| value.as_str() == source_documentation)
                .count(),
            0,
            "trait impl does not receive source docs",
        );
    }
}
