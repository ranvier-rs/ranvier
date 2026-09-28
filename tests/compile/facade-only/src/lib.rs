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

#[derive(Clone, ResourceRequirement)]
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
