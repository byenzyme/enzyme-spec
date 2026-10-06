//! One namespace across config files.
use std::path::Path;

#[test]
fn a_workspace_name_declared_in_two_files_is_an_error() {
    let a = enzyme_spec::parse(
        "workspace \"practice\" {\n  source markdown \"n\" { path \"/srv/a\" }\n}\n",
    )
    .unwrap();
    let b = enzyme_spec::parse(
        "workspace \"practice\" {\n  source markdown \"n\" { path \"/srv/b\" }\n}\n",
    )
    .unwrap();
    let error = enzyme_spec::resolve(vec![a.clone(), b], Path::new("/home/demo")).unwrap_err();
    assert!(
        format!("{error:#}").contains("duplicate workspace \"practice\""),
        "{error:#}"
    );
    enzyme_spec::resolve(vec![a], Path::new("/home/demo")).unwrap();
}
