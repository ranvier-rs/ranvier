use ranvier_core::outcome::Outcome;
use ranvier_core::transition::ResourceRequirement;
use ranvier_macros::transition;

struct AppResources;
struct ResourceEnvelope<T>(T);

impl<T> ResourceRequirement for ResourceEnvelope<T> where T: Send + Sync + 'static {}

#[transition(resources = ResourceEnvelope<AppResources>)]
async fn readiness() -> Outcome<bool, String> {
    Outcome::next(true)
}

fn main() {
    let _named_transition = readiness;
}
