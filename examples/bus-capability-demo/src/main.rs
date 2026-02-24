use ranvier_core::{Bus, Outcome};
use ranvier_macros::transition;
use ranvier_runtime::Axon;

#[transition(bus_allow = [i32])]
async fn allowed_read(_state: (), bus: &mut Bus) -> Outcome<i32, String> {
    match bus.get::<i32>() {
        Ok(value) => Outcome::next(*value),
        Err(err) => Outcome::fault(err.to_string()),
    }
}

#[transition(bus_allow = [i32], bus_deny = [String])]
async fn blocked_read(_state: (), bus: &mut Bus) -> Outcome<(), String> {
    match bus.get::<String>() {
        Ok(_) => Outcome::next(()),
        Err(err) => Outcome::fault(err.to_string()),
    }
}

#[tokio::main]
async fn main() {
    let mut bus = Bus::new();
    bus.insert(7_i32);
    bus.insert("secret".to_string());

    let allow_axon = Axon::<(), (), String>::new("AllowRead").then(allowed_read);
    let deny_axon = Axon::<(), (), String>::new("BlockRead").then(blocked_read);

    let allow_result = allow_axon.execute((), &(), &mut bus).await;
    let deny_result = deny_axon.execute((), &(), &mut bus).await;

    println!("allow_result={allow_result:?}");
    println!("deny_result={deny_result:?}");
}
