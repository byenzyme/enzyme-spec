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

fn programs(texts: &[&str]) -> Vec<enzyme_spec::Program> {
    texts
        .iter()
        .map(|t| enzyme_spec::parse(t).unwrap())
        .collect()
}

fn environment() -> enzyme_spec::Environment {
    enzyme_spec::Environment::new("/home/demo")
}

const MEETINGS: &str = r#"workspace "meetings" {
  source markdown "notes" { path "/srv/shared-notes" }
  learn questions from folder "Meetings"
}
"#;
const PEOPLE: &str = r#"workspace "people" {
  source markdown "notes" { path "/srv/shared-notes" }
  learn questions from folder "People"
}
"#;

#[test]
fn two_workspaces_may_share_one_markdown_folder() {
    let resolved =
        enzyme_spec::resolve(programs(&[MEETINGS, PEOPLE]), Path::new("/home/demo")).unwrap();
    assert_eq!(resolved.workspaces.len(), 2);
    let keys: Vec<String> = resolved
        .keyed_vaults()
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    // Each is keyed by name; the shared path names neither of them.
    assert_eq!(keys, ["workspace:meetings", "workspace:people"]);
    let config = enzyme_spec::config_value(&resolved);
    assert_eq!(
        config["vaults"]["workspace:meetings"]["entities"][0],
        "folder:Meetings"
    );
    assert_eq!(
        config["vaults"]["workspace:people"]["entities"][0],
        "folder:People"
    );
    assert!(config["vaults"].get("/srv/shared-notes").is_none());

    // Alone, a lone-Markdown workspace keeps its path-keyed identity too.
    let alone = enzyme_spec::resolve(programs(&[MEETINGS]), Path::new("/home/demo")).unwrap();
    let keys: Vec<String> = alone.keyed_vaults().into_iter().map(|(k, _)| k).collect();
    assert_eq!(keys, ["workspace:meetings", "/srv/shared-notes"]);
}

#[test]
fn a_vault_declaration_owns_its_path_beside_workspaces_reading_it() {
    let vault = "vault \"/srv/shared-notes\" { learn questions from folder \"Inbox\" }\n";
    let resolved =
        enzyme_spec::resolve(programs(&[MEETINGS, vault]), Path::new("/home/demo")).unwrap();
    let config = enzyme_spec::config_value(&resolved);
    assert_eq!(
        config["vaults"]["/srv/shared-notes"]["entities"][0],
        "folder:Inbox"
    );
    assert_eq!(
        config["vaults"]["workspace:meetings"]["entities"][0],
        "folder:Meetings"
    );

    // Two vault declarations of one path are still ambiguous.
    let error =
        enzyme_spec::resolve(programs(&[vault, vault]), Path::new("/home/demo")).unwrap_err();
    assert!(
        format!("{error:#}").contains("duplicate vault"),
        "{error:#}"
    );
    let namespace =
        enzyme_spec::resolve_namespace_in(programs(&[vault, vault, MEETINGS]), &environment())
            .unwrap();
    assert_eq!(
        namespace.problems[0].scope,
        enzyme_spec::Scope::Vault("/srv/shared-notes".into())
    );
    assert_eq!(namespace.vault_problems("/srv/shared-notes").len(), 1);
    assert!(namespace.workspace_problems("meetings").is_empty());
    assert!(
        namespace
            .program
            .vaults
            .iter()
            .all(|v| v.workspace.is_some())
    );
}

#[test]
fn one_invalid_workspace_does_not_stop_the_others() {
    let broken = r#"workspace "broken" {
  source markdown "notes" { path "relative/notes" }
}
"#;
    let unknown_profile = r#"workspace "typo" {
  source markdown "notes" { path "/srv/typo" }
  learn questions from folder "x" about no-such-profile
}
"#;
    let texts = [MEETINGS, broken, unknown_profile, PEOPLE];
    let namespace = enzyme_spec::resolve_namespace_in(programs(&texts), &environment()).unwrap();
    let names: Vec<&str> = namespace
        .program
        .workspaces
        .iter()
        .map(|w| w.name.as_str())
        .collect();
    assert_eq!(names, ["meetings", "people"]);
    assert!(namespace.workspace_problems("meetings").is_empty());
    let broken_problems = namespace.workspace_problems("broken");
    assert_eq!(broken_problems.len(), 1);
    assert!(
        broken_problems[0].message().contains("must be absolute"),
        "{}",
        broken_problems[0].message()
    );
    assert!(
        namespace.workspace_problems("typo")[0]
            .message()
            .contains("unknown profile no-such-profile")
    );
    // Strict resolution still reports the first problem.
    let error = enzyme_spec::resolve_in(programs(&texts), &environment()).unwrap_err();
    assert!(
        format!("{error:#}").contains("must be absolute"),
        "{error:#}"
    );
}

#[test]
fn a_duplicate_workspace_name_fails_only_that_name() {
    let again = MEETINGS.replace("Meetings", "Elsewhere");
    let namespace =
        enzyme_spec::resolve_namespace_in(programs(&[MEETINGS, &again, PEOPLE]), &environment())
            .unwrap();
    assert_eq!(namespace.workspace_problems("meetings").len(), 1);
    assert!(namespace.workspace_problems("people").is_empty());
    assert_eq!(namespace.program.workspaces.len(), 1);
}

#[test]
fn conflicting_definitions_fail_only_the_workspaces_that_use_them() {
    let profile_a = "profile decisions {\n  seek \"what was decided\"\n  notice [\"choices\"]\n}\n";
    let profile_b = "profile decisions {\n  seek \"something else\"\n  notice [\"choices\"]\n}\n";
    let user = r#"workspace "user" {
  source markdown "notes" { path "/srv/user" }
  learn questions from folder "x" about decisions
}
"#;
    let namespace = enzyme_spec::resolve_namespace_in(
        programs(&[profile_a, profile_b, user, MEETINGS]),
        &environment(),
    )
    .unwrap();
    assert!(namespace.workspace_problems("meetings").is_empty());
    let problems = namespace.workspace_problems("user");
    assert_eq!(problems.len(), 1);
    assert!(
        problems[0]
            .message()
            .contains("uses profile decisions, which config files define differently"),
        "{}",
        problems[0].message()
    );
    assert!(
        namespace
            .problems
            .iter()
            .any(|p| p.scope == enzyme_spec::Scope::Profile("decisions".into()))
    );
    // Strict resolution keeps reporting the conflict itself.
    let error =
        enzyme_spec::resolve_in(programs(&[profile_a, profile_b]), &environment()).unwrap_err();
    assert!(
        format!("{error:#}").contains("conflicting profile decisions across config files"),
        "{error:#}"
    );
}

#[test]
fn what_every_workspace_inherits_is_still_one_namespace() {
    let settings = "settings {\n  generation local\n}\n";
    let error = enzyme_spec::resolve_namespace_in(
        programs(&[settings, settings, MEETINGS]),
        &environment(),
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("duplicate setting"),
        "{error:#}"
    );
}

#[test]
fn a_file_that_does_not_parse_fails_only_what_it_could_declare() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("meetings.enzyme"), MEETINGS).unwrap();
    std::fs::write(dir.path().join("people.enzyme"), "workspace \"people\" {").unwrap();
    let namespace = enzyme_spec::load_namespace_in(dir.path(), &environment())
        .unwrap()
        .unwrap();
    assert!(namespace.workspace_problems("meetings").is_empty());
    let problems = namespace.workspace_problems("people");
    assert_eq!(problems.len(), 1);
    assert!(matches!(problems[0].scope, enzyme_spec::Scope::File(_)));
    assert!(
        problems[0].message().contains("invalid reading config"),
        "{}",
        problems[0].message()
    );
    // The strict loader is unchanged.
    assert!(enzyme_spec::load_directory_in(dir.path(), &environment()).is_err());
}

#[test]
fn a_candidate_is_checked_against_others_without_inheriting_their_problems() {
    let broken = r#"workspace "broken" {
  source markdown "notes" { path "relative/notes" }
}
"#;
    let candidate = enzyme_spec::parse(PEOPLE).unwrap();
    let namespace = enzyme_spec::resolve_with(
        programs(&[MEETINGS, broken]),
        candidate.clone(),
        Path::new("/home/demo"),
    )
    .unwrap();
    assert!(namespace.workspace_problems("people").is_empty());
    assert_eq!(namespace.workspace_problems("broken").len(), 1);

    // The candidate's own problem is an error.
    let bad = enzyme_spec::parse(&PEOPLE.replace("\"People\"", "\"People\" about nope")).unwrap();
    let error =
        enzyme_spec::resolve_with(programs(&[MEETINGS]), bad, Path::new("/home/demo")).unwrap_err();
    assert!(
        format!("{error:#}").contains("unknown profile nope"),
        "{error:#}"
    );

    // So is a problem it causes elsewhere: redefining a profile another file uses.
    let profile = "profile decisions {\n  seek \"what was decided\"\n  notice [\"choices\"]\n}\n";
    let user = r#"workspace "user" {
  source markdown "notes" { path "/srv/user" }
  learn questions from folder "x" about decisions
}
"#;
    let conflicting = enzyme_spec::parse(&profile.replace("what was decided", "other")).unwrap();
    let error = enzyme_spec::resolve_with(
        programs(&[profile, user]),
        conflicting,
        Path::new("/home/demo"),
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("conflicting profile decisions"),
        "{error:#}"
    );
}
