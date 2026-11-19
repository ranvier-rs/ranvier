// Actual shared branching string. Explicit model/error context below.
use ranvier::prelude::*;
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
enum OrderError {
    EncodingFailed,
}
#[derive(Debug, Clone, serde::Deserialize)]
struct Order {
    total: u32,
    reject_encoding: bool,
}
impl serde::Serialize for Order {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.reject_encoding {
            return Err(serde::ser::Error::custom("private-branch-sentinel"));
        }
        serializer.serialize_u32(self.total)
    }
}
#[transition]
async fn route_order(order: Order) -> Outcome<Order, OrderError> {
    if order.total > 10_000 {
        return match serde_json::to_value(&order) {
            Ok(payload) => Outcome::Branch("high_value".into(), Some(payload)),
            Err(_) => Outcome::Fault(OrderError::EncodingFailed),
        };
    }
    Outcome::Next(order)
}
#[tokio::test]
async fn public_branch_pattern_handles_encoding_failure() {
    for (total, reject_encoding) in [(100, true), (20000, false), (20000, true)] {
        let mut bus = Bus::new();
        let outcome = route_order
            .run(
                Order {
                    total,
                    reject_encoding,
                },
                &(),
                &mut bus,
            )
            .await;
        assert!(!format!("{outcome:?}").contains("private-branch-sentinel"));
        match outcome {
            Outcome::Next(order) => assert_eq!(order.total, 100),
            Outcome::Branch(target, payload) => {
                assert_eq!(total, 20000);
                assert!(!reject_encoding);
                assert_eq!(target, "high_value");
                assert_eq!(payload, Some(serde_json::json!(20000)));
            }
            Outcome::Fault(OrderError::EncodingFailed) => {
                assert_eq!(total, 20000);
                assert!(reject_encoding);
            }
            _ => panic!("unexpected branch outcome"),
        }
    }
    let axon = Axon::<Order, Order, OrderError>::new("public-branch").then(route_order);
    let report = axon
        .execute(
            Order {
                total: 20000,
                reject_encoding: true,
            },
            &(),
            &mut Bus::new(),
        )
        .await;
    assert!(matches!(
        report,
        ExecutionReport::Unpersisted {
            result: Outcome::Fault(OrderError::EncodingFailed),
            ..
        }
    ));
    assert!(!format!("{report:?}").contains("private-branch-sentinel"));
}
