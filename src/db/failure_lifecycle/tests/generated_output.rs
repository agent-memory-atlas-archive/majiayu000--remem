use super::*;

#[test]
fn generated_output_marker_changes_only_typed_generation_errors() {
    let source = anyhow::anyhow!("malformed stored evidence schema");
    assert_eq!(classify_failure_error(&source), FailureClass::Permanent);

    let generated = model_output_error(ExtractionTaskKind::ObservationExtract, source)
        .context("consume generated response");
    assert_eq!(classify_failure_error(&generated), FailureClass::Transient);

    let copied_text = anyhow::anyhow!(generated.to_string());
    // The marker cannot be forged by a provider/source error's display text.
    assert!(!copied_text.is::<ModelOutputError>());
    let source_text =
        anyhow::anyhow!("model_output_invalid kind=observation_extract: malformed source");
    assert_eq!(
        classify_failure_error(&source_text),
        FailureClass::Permanent
    );
}
