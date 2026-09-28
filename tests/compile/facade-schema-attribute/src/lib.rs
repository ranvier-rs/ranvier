use ranvier::prelude::*;

#[transition(schema)]
async fn facade_schema_transition(
    input: String,
    _resources: &(),
    _bus: &mut Bus,
) -> Outcome<String, String> {
    Outcome::next(input)
}

#[cfg(test)]
mod tests {
    use ranvier::Transition;
    use super::facade_schema_transition;

    #[test]
    fn facade_schema_attribute_generates_input_schema() {
        assert!(facade_schema_transition.input_schema().is_some());
    }
}
