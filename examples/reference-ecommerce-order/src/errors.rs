use ranvier_http::response::{IntoProblemDetail, ProblemDetail};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, Serialize, Deserialize, Clone)]
#[allow(dead_code)]
pub enum OrderError {
    #[error("Order not found: {0}")]
    NotFound(u64),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Payment failed: {0}")]
    PaymentFailed(String),

    #[error("Insufficient inventory for product: {0}")]
    InsufficientInventory(String),

    #[error("Shipping unavailable: {0}")]
    ShippingUnavailable(String),

    #[error("Compensation failed: {0}")]
    CompensationFailed(String),

    #[error("Unauthorized")]
    Unauthorized,

    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoProblemDetail for OrderError {
    fn into_problem_detail(&self) -> ProblemDetail {
        match self {
            OrderError::NotFound(id) => ProblemDetail::new(404, "Order Not Found")
                .with_detail(format!("Order with ID {id} was not found")),
            OrderError::InvalidInput(msg) => {
                ProblemDetail::new(400, "Invalid Input").with_detail(msg.clone())
            }
            OrderError::PaymentFailed(msg) => {
                ProblemDetail::new(402, "Payment Failed").with_detail(msg.clone())
            }
            OrderError::InsufficientInventory(product) => {
                ProblemDetail::new(409, "Insufficient Inventory")
                    .with_detail(format!("Not enough stock for product: {product}"))
            }
            OrderError::ShippingUnavailable(msg) => {
                ProblemDetail::new(503, "Shipping Unavailable").with_detail(msg.clone())
            }
            OrderError::CompensationFailed(msg) => {
                ProblemDetail::new(500, "Compensation Failed").with_detail(msg.clone())
            }
            OrderError::Unauthorized => ProblemDetail::new(401, "Unauthorized"),
            OrderError::Internal(msg) => {
                ProblemDetail::new(500, "Internal Server Error").with_detail(msg.clone())
            }
        }
    }
}
