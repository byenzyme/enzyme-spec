//! The README's example programs must stay valid.

#[test]
fn readme_examples_parse_resolve_and_render_canonically() {
    let readme = include_str!("../README.md");
    let environment =
        enzyme_spec::Environment::new("/home/demo").with_enzyme_home("/home/demo/.enzyme");
    let mut examples = 0;
    for block in readme.split("```enzyme\n").skip(1) {
        let source = &block[..block.find("```").unwrap()];
        let program = enzyme_spec::parse(source).unwrap();
        let rendered = enzyme_spec::render_program(&program);
        assert_eq!(enzyme_spec::parse(&rendered).unwrap(), program);
        enzyme_spec::resolve_in(vec![program], &environment).unwrap();
        examples += 1;
    }
    assert!(examples >= 2, "README has enzyme examples");
}
