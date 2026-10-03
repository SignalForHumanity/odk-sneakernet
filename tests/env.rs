//! Separate test binary: it sets ODK_SERVER, which other tests must not see.

#[test]
fn server_url_can_come_from_environment() {
    let root = std::env::temp_dir().join(format!("odk-sn-env-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    // SAFETY: the only test in this binary.
    unsafe {
        std::env::set_var(
            "ODK_SERVER",
            "https://central.example.org/v1/key/T/projects/3",
        )
    };
    let args: Vec<String> = ["push", "--dry-run", root.to_str().unwrap()]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut out = Vec::new();
    let code = odk_sneakernet::run(&args, &mut out);
    let out = String::from_utf8(out).unwrap();
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("dry run: 0 submissions"), "{out}");
}
