#![allow(dead_code)]
use ranvier_runtime::{ExecutionReport, StreamInitializationReport};
/// Test-only extraction: accept exactly the known unpersisted variants used by
/// ownership fixtures. Any uncertainty, rejection or unexpected cleanup is a failure.
pub fn initialized<Out, Value, E>(
    report: StreamInitializationReport<Out, Value, E>,
) -> Result<Value, ()> {
    match report {
        StreamInitializationReport::Ready {
            prefix:
                ExecutionReport::Unpersisted {
                    result: (),
                    cleanup: None,
                },
            value,
        } => Ok(value),
        StreamInitializationReport::Failed {
            prefix:
                ExecutionReport::Unpersisted {
                    result: (),
                    cleanup: None,
                },
            ..
        } => Err(()),
        _ => panic!("unexpected prefix report in unpersisted initialization fixture"),
    }
}
pub fn ready<Out, Value, E>(report: StreamInitializationReport<Out, Value, E>) -> Value {
    initialized(report).unwrap_or_else(|_| panic!("expected initialized stream"))
}
