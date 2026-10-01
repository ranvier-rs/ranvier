//! # Bus Capability Control
//!
//! Demonstrates runtime Bus access control declared with `bus_allow` and `bus_deny` attributes.
//!
//! ## Run
//! ```bash
//! cargo run -p bus-capability-demo
//! ```
//!
//! ## Key Concepts
//! - `#[transition(bus_allow = [T])]` for type whitelisting
//! - `#[transition(bus_deny = [T])]` for type blacklisting
//! - Compile-time validation of the declared type list
//! - Runtime enforcement for actual shared-state access

use ranvier_core::{Bus, Outcome};
use ranvier_macros::transition;
use ranvier_runtime::Axon;

fn local_demo_outcome<T, E>(
    report: ranvier_runtime::ExecutionReport<Outcome<T, E>>,
) -> Result<Outcome<T, E>, String> {
    if report
        .cleanup()
        .is_some_and(|cleanup| !cleanup.is_complete())
    {
        return Err("Execution requires reconciliation before retry".into());
    }
    match report {
        ranvier_runtime::ExecutionReport::Unpersisted { result, .. } => Ok(result),
        _ => Err("Execution requires reconciliation before retry".into()),
    }
}

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
async fn main() -> Result<(), String> {
    let mut bus = Bus::new();
    bus.insert(7_i32);
    bus.insert("secret".to_string());

    let allow_axon = Axon::simple::<String>("AllowRead").then(allowed_read);
    let deny_axon = Axon::simple::<String>("BlockRead").then(blocked_read);

    let allow_result = local_demo_outcome(allow_axon.execute((), &(), &mut bus).await)?;
    let mut deny_bus = Bus::new();
    deny_bus.insert("secret".to_string());
    let deny_result = local_demo_outcome(deny_axon.execute((), &(), &mut deny_bus).await)?;

    if !matches!(allow_result, Outcome::Next(7)) {
        return Err("Allowed capability scenario failed".into());
    }
    if !matches!(deny_result, Outcome::Fault(ref error) if error.contains("Bus access denied")) {
        return Err("Denied capability scenario failed".into());
    }
    println!("Allowed read accepted; forbidden read denied");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn real_macro_policy_enforces_access_and_restores_scope() {
        let mut bus = Bus::new();
        bus.insert(7_i32);
        bus.insert("private-value".to_string());
        let allow = Axon::simple::<String>("allow").then(allowed_read);
        assert!(matches!(
            local_demo_outcome(allow.execute((), &(), &mut bus).await).unwrap(),
            Outcome::Next(7)
        ));
        let deny = Axon::simple::<String>("deny").then(blocked_read);
        let result = local_demo_outcome(deny.execute((), &(), &mut bus).await).unwrap();
        assert!(
            matches!(result, Outcome::Fault(ref error) if error.contains("Bus access denied") && !error.contains("private-value"))
        );
        assert_eq!(bus.get::<String>().unwrap(), "private-value");
    }
    // This transition deliberately attempts denied operations for a guard test;
    // its unit outcome is not acknowledgement of the attempted String writes.
    #[transition(bus_allow = [i32], bus_deny = [String])]
    async fn attempt_guarded_writes(_state: (), bus: &mut Bus) -> Outcome<(), String> {
        bus.insert(42_i32);
        bus.insert("denied-local".to_string());
        bus.insert_shared("denied-shared".to_string());
        bus.provide("denied-provide".to_string());
        bus.provide_shared("denied-provide-shared".to_string());
        Outcome::Next(())
    }

    #[tokio::test]
    async fn macro_policy_blocks_write_aliases_and_preserves_existing_storage() {
        for shared in [false, true] {
            for existing in [false, true] {
                let mut bus = Bus::new();
                if existing {
                    if shared {
                        bus.insert_shared("original".to_string());
                    } else {
                        bus.insert("original".to_string());
                    }
                }
                let axon =
                    Axon::simple::<String>("guarded-write-test").then(attempt_guarded_writes);
                assert!(matches!(
                    local_demo_outcome(axon.execute((), &(), &mut bus).await).unwrap(),
                    Outcome::Next(())
                ));
                assert_eq!(
                    bus.read::<String>().map(String::as_str),
                    if existing { Some("original") } else { None }
                );
                assert_eq!(*bus.get::<i32>().unwrap(), 42);
                // A denied local insertion must not remove an existing shared
                // entry or make a local value shareable in the next fork.
                let fork = bus.fork_for_parallel();
                assert_eq!(
                    fork.read::<String>().map(String::as_str),
                    if existing && shared {
                        Some("original")
                    } else {
                        None
                    }
                );
            }
        }
    }
    #[tokio::test]
    async fn missing_allowed_capability_is_fault() {
        let axon = Axon::simple::<String>("missing").then(allowed_read);
        assert!(matches!(
            local_demo_outcome(axon.execute((), &(), &mut Bus::new()).await).unwrap(),
            Outcome::Fault(_)
        ));
    }
}
