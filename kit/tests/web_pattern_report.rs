// Extracted public pattern strings with explicit supporting transition context.

#[rustfmt::skip] // Preserve the exact exported source for drift checking.
mod en {
use ranvier::prelude::*;
use ranvier::{ExecutionReport, CompletionConfirmation};
use ranvier_runtime::persistence::{InMemoryPersistenceStore, PersistenceHandle, PersistenceStore, PersistenceTraceId, PersistenceAutoComplete};

#[derive(Clone)]
struct ValidateInput;
#[async_trait::async_trait]
impl Transition<String, String> for ValidateInput {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: String, _: &(), _: &mut Bus) -> Outcome<String, String> { Outcome::Next(input) }
}


#[derive(Clone)]
struct ProcessData;
#[async_trait::async_trait]
impl Transition<String, String> for ProcessData {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: String, _: &(), _: &mut Bus) -> Outcome<String, String> { Outcome::Next(input) }
}


#[derive(Clone)]
struct FormatOutput;
#[async_trait::async_trait]
impl Transition<String, String> for FormatOutput {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: String, _: &(), _: &mut Bus) -> Outcome<String, String> { Outcome::Next(input) }
}

#[tokio::test]
async fn basic_report() {
    let input = "pattern".to_string();
    let mut bus = Bus::new();
let axon = Axon::<String, String, String>::new("pipeline")
    .then(ValidateInput)
    .then(ProcessData)
    .then(FormatOutput);

let report = axon.execute(input, &(), &mut bus).await;
// Local demo: inspect domain and durability together before retry.
match report {
    ExecutionReport::Unpersisted { result: Outcome::Next(output), .. } => println!("{output}"),
    _ => eprintln!("Execution requires review before retry"),
}
}
#[tokio::test]
async fn explicit_completion_owner() {
    let store = InMemoryPersistenceStore::new();
    let handle = PersistenceHandle::from_store(store.clone());
    let input = "pattern".to_string();
    let axon = Axon::<String, String, String>::new("pattern").then(ValidateInput);
let mut bus = Bus::new();
bus.insert(handle.clone());
bus.insert(PersistenceTraceId::new("trace-001"));
bus.insert(PersistenceAutoComplete(false));
let report = axon.execute(input, &(), &mut bus).await;
// AutoComplete(false) records events without certifying terminal completion.
let events_recorded = matches!(&report,
    ExecutionReport::Recorded { completion: CompletionConfirmation::NotRequested, .. }
) && report.cleanup().is_none_or(|cleanup| cleanup.is_complete());
if !events_recorded {
    eprintln!("Execution requires reconciliation before retry");
}
// The caller still owns terminal completion; this is not durable workflow success.
    assert!(events_recorded);
    assert_eq!(store.load("trace-001").await.unwrap().unwrap().completion, None);
}
}

#[rustfmt::skip] // Preserve the exact exported source for drift checking.
mod ko {
use ranvier::prelude::*;
use ranvier::{ExecutionReport, CompletionConfirmation};
use ranvier_runtime::persistence::{InMemoryPersistenceStore, PersistenceHandle, PersistenceStore, PersistenceTraceId, PersistenceAutoComplete};

#[derive(Clone)]
struct ValidateInput;
#[async_trait::async_trait]
impl Transition<String, String> for ValidateInput {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: String, _: &(), _: &mut Bus) -> Outcome<String, String> { Outcome::Next(input) }
}


#[derive(Clone)]
struct ProcessData;
#[async_trait::async_trait]
impl Transition<String, String> for ProcessData {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: String, _: &(), _: &mut Bus) -> Outcome<String, String> { Outcome::Next(input) }
}


#[derive(Clone)]
struct FormatOutput;
#[async_trait::async_trait]
impl Transition<String, String> for FormatOutput {
    type Error = String;
    type Resources = ();
    async fn run(&self, input: String, _: &(), _: &mut Bus) -> Outcome<String, String> { Outcome::Next(input) }
}

#[tokio::test]
async fn basic_report() {
    let input = "pattern".to_string();
    let mut bus = Bus::new();
let axon = Axon::<String, String, String>::new("pipeline")
    .then(ValidateInput)
    .then(ProcessData)
    .then(FormatOutput);

let report = axon.execute(input, &(), &mut bus).await;
// Local demo: inspect domain and durability together before retry.
match report {
    ExecutionReport::Unpersisted { result: Outcome::Next(output), .. } => println!("{output}"),
    _ => eprintln!("Execution requires review before retry"),
}
}
#[tokio::test]
async fn explicit_completion_owner() {
    let store = InMemoryPersistenceStore::new();
    let handle = PersistenceHandle::from_store(store.clone());
    let input = "pattern".to_string();
    let axon = Axon::<String, String, String>::new("pattern").then(ValidateInput);
let mut bus = Bus::new();
bus.insert(handle.clone());
bus.insert(PersistenceTraceId::new("trace-001"));
bus.insert(PersistenceAutoComplete(false));
let report = axon.execute(input, &(), &mut bus).await;
// AutoComplete(false) records events without certifying terminal completion.
let events_recorded = matches!(&report,
    ExecutionReport::Recorded { completion: CompletionConfirmation::NotRequested, .. }
) && report.cleanup().is_none_or(|cleanup| cleanup.is_complete());
if !events_recorded {
    eprintln!("Execution requires reconciliation before retry");
}
// The caller still owns terminal completion; this is not durable workflow success.
    assert!(events_recorded);
    assert_eq!(store.load("trace-001").await.unwrap().unwrap().completion, None);
}
}
