//! Widget shapes.

/// Shared padding.
pub const PAD: usize = 2;

/// Hidden helper.
fn pad(value: usize) -> usize {
    value + PAD
}

/// First paragraph of docs.
/// Second paragraph of docs.
/// Third paragraph of docs.
/// Fourth paragraph of docs.
/// Fifth paragraph of docs.
/// Sixth paragraph of docs.
/// Seventh paragraph of docs.
/// Eighth paragraph of docs.
pub fn build() -> usize {
    pad(40)
}
