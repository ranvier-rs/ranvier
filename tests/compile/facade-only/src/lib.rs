//! Compile-only proof for the documented facade-only dependency path.

use ranvier::prelude::*;

#[transition]
async fn no_parameters() -> Outcome<(), String> {
    Outcome::next(())
}

pub fn no_parameter_axon() -> Axon<(), (), String, ()> {
    Axon::<(), (), String, ()>::new("facade-zero-parameter").then(no_parameters)
}

#[transition]
async fn input_only(input: i32) -> Outcome<i32, String> {
    Outcome::next(input)
}

pub fn input_only_axon() -> Axon<i32, i32, String, ()> {
    Axon::<i32, i32, String, ()>::new("facade-input-only").then(input_only)
}

#[transition]
async fn resources_only(_resources: &AppResources) -> Outcome<(), String> {
    Outcome::next(())
}

pub fn resources_only_axon() -> Axon<(), (), String, AppResources> {
    Axon::<(), (), String, AppResources>::new("facade-resources-only").then(resources_only)
}

#[transition]
async fn bus_only(_bus: &mut Bus) -> Outcome<(), String> {
    Outcome::next(())
}

pub fn bus_only_axon() -> Axon<(), (), String, ()> {
    Axon::<(), (), String, ()>::new("facade-bus-only").then(bus_only)
}

#[transition]
async fn input_and_bus(input: i32, _bus: &mut Bus) -> Outcome<i32, String> {
    Outcome::next(input)
}

pub fn input_and_bus_axon() -> Axon<i32, i32, String, ()> {
    Axon::<i32, i32, String, ()>::new("facade-input-bus").then(input_and_bus)
}

#[transition]
async fn input_and_resources(input: i32, _resources: &AppResources) -> Outcome<i32, String> {
    Outcome::next(input)
}

pub fn input_and_resources_axon() -> Axon<i32, i32, String, AppResources> {
    Axon::<i32, i32, String, AppResources>::new("facade-input-resources")
        .then(input_and_resources)
}

#[transition]
async fn all_roles(
    input: i32,
    _resources: &AppResources,
    _bus: &mut Bus,
) -> Outcome<i32, String> {
    Outcome::next(input)
}

pub fn all_roles_axon() -> Axon<i32, i32, String, AppResources> {
    Axon::<i32, i32, String, AppResources>::new("facade-all-roles").then(all_roles)
}

#[transition]
async fn resources_and_bus(_resources: &AppResources, _bus: &mut Bus) -> Outcome<(), String> {
    Outcome::next(())
}

pub fn resources_and_bus_axon() -> Axon<(), (), String, AppResources> {
    Axon::<(), (), String, AppResources>::new("facade-resources-bus").then(resources_and_bus)
}

#[cfg(feature = "streaming-contract")]
struct OneItemStream(Option<i32>);

#[cfg(feature = "streaming-contract")]
impl ranvier::core::__macro_support::futures_core::Stream for OneItemStream {
    type Item = i32;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Ready(self.as_mut().get_mut().0.take())
    }
}

#[cfg(feature = "streaming-contract")]
#[streaming_transition]
async fn stream_only(
    input: i32,
) -> Result<impl ranvier::core::__macro_support::futures_core::Stream<Item = i32> + Send, String> {
    Ok(OneItemStream(Some(input)))
}

#[cfg(feature = "streaming-contract")]
pub fn streaming_axon() -> StreamingAxon<i32, i32, String, ()> {
    Axon::<i32, i32, String, ()>::new("facade-streaming").then_stream(stream_only)
}

#[cfg(feature = "streaming-contract")]
#[streaming_transition]
async fn stream_no_roles(
) -> Result<impl ranvier::core::__macro_support::futures_core::Stream<Item = i32> + Send, String> {
    Ok(OneItemStream(Some(0)))
}

#[cfg(feature = "streaming-contract")]
pub fn streaming_no_roles_axon() -> StreamingAxon<(), i32, String, ()> {
    Axon::<(), (), String, ()>::new("facade-streaming-no-roles").then_stream(stream_no_roles)
}

#[cfg(feature = "streaming-contract")]
#[streaming_transition]
async fn stream_resources_only(
    _resources: &AppResources,
) -> Result<impl ranvier::core::__macro_support::futures_core::Stream<Item = i32> + Send, String> {
    Ok(OneItemStream(Some(0)))
}

#[cfg(feature = "streaming-contract")]
pub fn streaming_resources_only_axon() -> StreamingAxon<(), i32, String, AppResources> {
    Axon::<(), (), String, AppResources>::new("facade-streaming-resources")
        .then_stream(stream_resources_only)
}

#[cfg(feature = "streaming-contract")]
#[streaming_transition(resources = AppResources)]
async fn stream_resources_override(
) -> Result<impl ranvier::core::__macro_support::futures_core::Stream<Item = i32> + Send, String> {
    Ok(OneItemStream(Some(0)))
}

#[cfg(feature = "streaming-contract")]
pub fn streaming_resources_override_axon() -> StreamingAxon<(), i32, String, AppResources> {
    Axon::<(), (), String, AppResources>::new("facade-streaming-resource-override")
        .then_stream(stream_resources_override)
}

#[cfg(feature = "streaming-contract")]
#[streaming_transition(resources = AppResources)]
async fn stream_resources_override_and_input(
    input: i32,
) -> Result<impl ranvier::core::__macro_support::futures_core::Stream<Item = i32> + Send, String> {
    Ok(OneItemStream(Some(input)))
}

#[cfg(feature = "streaming-contract")]
pub fn streaming_resources_override_and_input_axon(
) -> StreamingAxon<i32, i32, String, AppResources> {
    Axon::<i32, i32, String, AppResources>::new("facade-streaming-resource-override-input")
        .then_stream(stream_resources_override_and_input)
}

#[cfg(feature = "streaming-contract")]
#[streaming_transition(resources = AppResources)]
async fn stream_resources_override_and_bus(
    _bus: &mut Bus,
) -> Result<impl ranvier::core::__macro_support::futures_core::Stream<Item = i32> + Send, String> {
    Ok(OneItemStream(Some(0)))
}

#[cfg(feature = "streaming-contract")]
pub fn streaming_resources_override_and_bus_axon(
) -> StreamingAxon<(), i32, String, AppResources> {
    Axon::<(), (), String, AppResources>::new("facade-streaming-resource-override-bus")
        .then_stream(stream_resources_override_and_bus)
}

#[cfg(feature = "streaming-contract")]
#[streaming_transition(resources = AppResources)]
async fn stream_resources_override_input_and_bus(
    input: i32,
    _bus: &mut Bus,
) -> Result<impl ranvier::core::__macro_support::futures_core::Stream<Item = i32> + Send, String> {
    Ok(OneItemStream(Some(input)))
}

#[cfg(feature = "streaming-contract")]
pub fn streaming_resources_override_input_and_bus_axon(
) -> StreamingAxon<i32, i32, String, AppResources> {
    Axon::<i32, i32, String, AppResources>::new("facade-streaming-resource-override-input-bus")
        .then_stream(stream_resources_override_input_and_bus)
}

#[cfg(feature = "streaming-contract")]
#[streaming_transition]
async fn stream_bus_only(
    _bus: &mut Bus,
) -> Result<impl ranvier::core::__macro_support::futures_core::Stream<Item = i32> + Send, String> {
    Ok(OneItemStream(Some(0)))
}

#[cfg(feature = "streaming-contract")]
pub fn streaming_bus_only_axon() -> StreamingAxon<(), i32, String, ()> {
    Axon::<(), (), String, ()>::new("facade-streaming-bus").then_stream(stream_bus_only)
}

#[cfg(feature = "streaming-contract")]
#[streaming_transition]
async fn stream_input_and_resources(
    input: i32,
    _resources: &AppResources,
) -> Result<impl ranvier::core::__macro_support::futures_core::Stream<Item = i32> + Send, String> {
    Ok(OneItemStream(Some(input)))
}

#[cfg(feature = "streaming-contract")]
pub fn streaming_input_and_resources_axon() -> StreamingAxon<i32, i32, String, AppResources> {
    Axon::<i32, i32, String, AppResources>::new("facade-streaming-input-resources")
        .then_stream(stream_input_and_resources)
}

#[cfg(feature = "streaming-contract")]
#[streaming_transition]
async fn stream_input_and_bus(
    input: i32,
    _bus: &mut Bus,
) -> Result<impl ranvier::core::__macro_support::futures_core::Stream<Item = i32> + Send, String> {
    Ok(OneItemStream(Some(input)))
}

#[cfg(feature = "streaming-contract")]
pub fn streaming_input_and_bus_axon() -> StreamingAxon<i32, i32, String, ()> {
    Axon::<i32, i32, String, ()>::new("facade-streaming-input-bus")
        .then_stream(stream_input_and_bus)
}

#[cfg(feature = "streaming-contract")]
#[streaming_transition]
async fn stream_resources_and_bus(
    _resources: &AppResources,
    _bus: &mut Bus,
) -> Result<impl ranvier::core::__macro_support::futures_core::Stream<Item = i32> + Send, String> {
    Ok(OneItemStream(Some(0)))
}

#[cfg(feature = "streaming-contract")]
pub fn streaming_resources_and_bus_axon() -> StreamingAxon<(), i32, String, AppResources> {
    Axon::<(), (), String, AppResources>::new("facade-streaming-resources-bus")
        .then_stream(stream_resources_and_bus)
}

#[cfg(feature = "streaming-contract")]
#[streaming_transition]
async fn stream_all_roles(
    input: i32,
    _resources: &AppResources,
    _bus: &mut Bus,
) -> Result<impl ranvier::core::__macro_support::futures_core::Stream<Item = i32> + Send, String> {
    Ok(OneItemStream(Some(input)))
}

#[cfg(feature = "streaming-contract")]
pub fn streaming_all_roles_axon() -> StreamingAxon<i32, i32, String, AppResources> {
    Axon::<i32, i32, String, AppResources>::new("facade-streaming-all-roles")
        .then_stream(stream_all_roles)
}

#[derive(Clone)]
pub struct AppResources {
    suffix: &'static str,
}

#[derive(Clone)]
struct RequestContext(&'static str);

#[transition(bus_allow = [RequestContext])]
async fn decide(_input: (), resources: &AppResources, bus: &mut Bus) -> Outcome<String, String> {
    let request = match bus.get::<RequestContext>() {
        Ok(request) => request,
        Err(error) => return Outcome::fault(error.to_string()),
    };
    Outcome::next(format!("{}{}", request.0, resources.suffix))
}

#[transition(resources = AppResources)]
async fn finalize(input: String) -> Outcome<String, String> {
    Outcome::next(input.to_uppercase())
}

#[cfg(feature = "schema-contract")]
#[transition(schema)]
async fn schema_from_facade(input: String) -> Outcome<String, String> {
    Outcome::next(input)
}

pub fn candidate_axon() -> Axon<(), String, String, AppResources> {
    Axon::<(), (), String, AppResources>::new("facade-contract")
        .then(decide)
        .then(finalize)
}

pub fn compile_native_and_hybrid_entry_points() {
    let native = Ranvier::http::<AppResources>().route("/decide", candidate_axon());
    let _tower_hyper_service = native.into_raw_service(AppResources { suffix: "!" });
}
