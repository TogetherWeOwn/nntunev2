//! Shared synthetic vectors for the ingest and stream acceptance targets.
//! The caller supplies a test macro, so every vector runs independently.

pub fn id(bytes: usize) -> String {
    // One '@', nonempty sides, and exactly `bytes` printable ASCII bytes.
    format!("{}@b", "a".repeat(bytes - 2))
}

macro_rules! identity_cases {
    ($case:ident) => {
        $case!(minimal, "a@b".to_owned(), true);
        $case!(printable_edges, "!~@synthetic.invalid".to_owned(), true);
        $case!(bytes_249, $crate::identity_vectors::id(249), true);
        $case!(bytes_250, $crate::identity_vectors::id(250), true);
        $case!(bytes_251, $crate::identity_vectors::id(251), false);
        $case!(bytes_512, $crate::identity_vectors::id(512), false);
        $case!(bytes_513, $crate::identity_vectors::id(513), false);
        $case!(bracketed, "<seg@synthetic.invalid>".to_owned(), false);
        $case!(leading_space, " seg@synthetic.invalid".to_owned(), false);
        $case!(trailing_space, "seg@synthetic.invalid ".to_owned(), false);
        $case!(
            padded_bracketed,
            "  <seg@synthetic.invalid>  ".to_owned(),
            false
        );
        $case!(leading_tab, "\tseg@synthetic.invalid".to_owned(), false);
        $case!(trailing_cr, "seg@synthetic.invalid\r".to_owned(), false);
        $case!(leading_lf, "\nseg@synthetic.invalid".to_owned(), false);
        $case!(embedded_cr, "se\rg@synthetic.invalid".to_owned(), false);
        $case!(embedded_lf, "seg@synthetic.in\nvalid".to_owned(), false);
        $case!(injection, "x@y>\r\nQUIT\r\n<z".to_owned(), false);
        $case!(internal_space, "se g@synthetic.invalid".to_owned(), false);
        $case!(internal_bracket, "se<g@synthetic.invalid".to_owned(), false);
        $case!(double_at, "seg@@synthetic.invalid".to_owned(), false);
        $case!(empty_local, "@synthetic.invalid".to_owned(), false);
        $case!(empty_domain, "seg@".to_owned(), false);
        $case!(no_at, "not-a-message-id".to_owned(), false);
        $case!(empty, String::new(), false);
        $case!(nul, "se\0g@synthetic.invalid".to_owned(), false);
        $case!(del, "seg@synthetic.\x7finvalid".to_owned(), false);
        $case!(non_ascii, "s\u{e9}g@synthetic.invalid".to_owned(), false);
    };
}
