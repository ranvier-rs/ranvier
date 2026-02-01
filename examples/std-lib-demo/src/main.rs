use ranvier_core::prelude::*;
use ranvier_std::prelude::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    println!("Running Ranvier Standard Library Demo...");

    // 1. Math Demo: (5 + 10) * 2 = 30
    let math_pipeline = Axon::start(5, "Math Demo")
        .then(MathNode::new(MathOperation::Add, 10))
        .then(MathNode::new(MathOperation::Mul, 2));

    let mut bus = Bus::new();
    let result = math_pipeline.execute(&mut bus).await;
    println!("Math Result: {:?}", result);

    // 2. String Demo: "hello" -> UPPER -> Append " WORLD"
    let string_pipeline = Axon::start("hello".to_string(), "String Demo")
        .then(StringNode::new(StringOperation::ToUpper))
        .then(StringNode::new(StringOperation::Append(
            " WORLD".to_string(),
        )));

    let result = string_pipeline.execute(&mut bus).await;
    println!("String Result: {:?}", result);

    // 3. Logic Demo (Existing)
    let filter = FilterNode::new(|s: &String| s.len() > 5);
    let switch = SwitchNode::new(|s: &String| {
        if s.contains("Hello") {
            "greeting".to_string()
        } else {
            "other".to_string()
        }
    });

    let logic_pipeline = Axon::start("Hello Ranvier".to_string(), "Logic Demo")
        .then(LogNode::new("Start", "info"))
        .then(filter)
        .then(switch);

    let result = logic_pipeline.execute(&mut bus).await;
    println!("Logic Result: {:?}", result);

    Ok(())
}
