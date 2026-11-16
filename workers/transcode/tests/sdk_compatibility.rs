#[test]
fn sdk_http_methods_preserve_transcode_verbs() {
    for (verb, expected) in [
        ("GET", worker::Method::Get),
        ("POST", worker::Method::Post),
        ("PUT", worker::Method::Put),
    ] {
        let method = worker::Method::from(verb.to_owned());
        assert_eq!(method, expected);
        assert_eq!(method.as_ref(), verb);
        assert_eq!(String::from(method), verb);
    }
}
