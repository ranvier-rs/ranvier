use ranvier_core::outcome::Outcome;
use ranvier_macros::transition;

struct ResourceEnvelope<T>(T);
struct AppResources;

#[transition(resources = ResourceEnvelope<AppResources>)]
async fn readiness() -> Outcome<bool, String> {
    Outcome::next(true)
}

fn main() {
    let _named_transition = readiness;
}
