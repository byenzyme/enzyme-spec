//! The README's example program must stay valid.
use std::path::Path;

#[test]
fn readme_example_parses_resolves_and_renders_canonically() {
    let readme = include_str!("../README.md");
    let start = readme.find("```enzyme\n").expect("README has an enzyme example") + "```enzyme\n".len();
    let source = &readme[start..start + readme[start..].find("```").unwrap()];
    let program = enzyme_spec::parse(source).unwrap();
    let rendered = enzyme_spec::render_program(&program);
    assert_eq!(enzyme_spec::parse(&rendered).unwrap(), program);
    enzyme_spec::resolve(vec![program], Path::new("/home/demo")).unwrap();
}
