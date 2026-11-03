//! Standalone acceptance package for the sandboxed offline checkout workflow.
//!
//! Kept outside the main workspace so the acceptance run proves a fresh,
//! dependency-free package builds from the real checkout only.

/// Returns the product-visible completion message of a checkout acceptance run.
pub fn checkout_completion_message() -> &'static str {
    "仓库构建验收完成 😀"
}

#[cfg(test)]
mod tests {
    use super::checkout_completion_message;

    #[test]
    fn reports_checkout_completion() {
        assert_eq!(checkout_completion_message(), "仓库构建验收完成 😀");
        assert!(checkout_completion_message().ends_with('😀'));
    }

    // Negative control: compiled only under the `negative_control` feature, so a
    // plain `cargo test` stays green while the control run must fail.
    #[cfg(feature = "negative_control")]
    #[test]
    fn negative_control_fails_on_purpose() {
        assert_eq!(1, 2);
    }
}
