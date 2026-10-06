//! `source kind` definitions: grammar, expansion in `resolve`, rendering.
use enzyme_spec::*;
use std::path::Path;

const MAIL_KIND: &str = r#"
source kind google-mail {
  needs account
  accepts query, backfill days
  database "{home}/workspaces/{workspace}/ledger.db"
  query """
SELECT id, sender, sent_ms, body
FROM mail
WHERE account = {account} AND workspace = {workspace} -- not {account} in comments
"""
  id "id"
  who "sender"
  when "sent_ms" unit ms
  what "body"
}
"#;

const MAIL_WORKSPACE: &str = r#"
workspace "practice" {
  source google-mail "mail" {
    account "me@example.com"
    query "-in:spam"
    backfill days 365
  }

  learn questions from source "mail" including who links
}
"#;

fn environment() -> Environment {
    Environment::new("/home/demo").with_enzyme_home("/home/demo/.margins")
}

fn resolve_text(texts: &[&str], environment: &Environment) -> anyhow::Result<Program> {
    let programs = texts
        .iter()
        .map(|text| parse(text))
        .collect::<anyhow::Result<Vec<_>>>()?;
    resolve_in(programs, environment)
}

fn error(texts: &[&str], environment: &Environment) -> String {
    match resolve_text(texts, environment) {
        Ok(_) => panic!("expected an error for {texts:?}"),
        Err(error) => format!("{error:#}"),
    }
}

fn sqlite<'a>(program: &'a Program, workspace: &str, name: &str) -> &'a SqliteSource {
    program
        .workspaces
        .iter()
        .find(|w| w.name == workspace)
        .unwrap()
        .sources
        .iter()
        .find_map(|source| match source {
            Source::Sqlite(source) if source.name == name => Some(source),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{name} did not expand to SQLite"))
}

#[test]
fn definitions_parse_render_and_round_trip() {
    let program = parse(&format!("{MAIL_KIND}{MAIL_WORKSPACE}")).unwrap();
    let kind = &program.source_kinds["google-mail"];
    assert_eq!(kind.needs, ["account"]);
    assert_eq!(kind.accepts, ["query", "backfill days"]);
    assert_eq!(kind.template.db, "{home}/workspaces/{workspace}/ledger.db");
    // Before resolution the declaration is still a host source.
    assert!(matches!(program.workspaces[0].sources[0], Source::Host(_)));

    let rendered = render_program(&program);
    assert!(rendered.contains("source kind google-mail {\n  needs account\n  accepts query, backfill days\n  database \"{home}/workspaces/{workspace}/ledger.db\"\n"), "{rendered}");
    let reparsed = parse(&rendered).unwrap_or_else(|e| panic!("{e:#}\n{rendered}"));
    assert_eq!(reparsed, program);
    assert_eq!(render_program(&reparsed), rendered, "rendering is canonical");
}

#[test]
fn field_lists_span_lines_after_commas() {
    let program = parse(
        r#"source kind k {
  needs account,
    calendar id
  accepts a b, c
  database "/d.db"
  query "SELECT 1 AS id, 1 AS t, 'x' AS w FROM x WHERE a = {account} AND c = {calendar id}"
  id "id" when "t" unit s what "w"
}"#,
    )
    .unwrap();
    let kind = &program.source_kinds["k"];
    assert_eq!(kind.needs, ["account", "calendar id"]);
    assert_eq!(kind.accepts, ["a b", "c"]);
}

#[test]
fn declarations_expand_to_native_sqlite_in_resolve() {
    let resolved = resolve_text(&[MAIL_KIND, MAIL_WORKSPACE], &environment()).unwrap();
    let mail = sqlite(&resolved, "practice", "mail");
    assert_eq!(mail.db, "/home/demo/.margins/workspaces/practice/ledger.db");
    assert!(mail.query.contains("WHERE account = 'me@example.com' AND workspace = 'practice'"));
    assert!(mail.query.contains("-- not {account} in comments"));
    assert_eq!(mail.who, SqliteWho::Columns { columns: vec!["sender".into()] });
    // Readings of the declared name resolve against the expanded source,
    // and the engine configuration sees an ordinary SQLite source.
    let config = config_value(&resolved);
    let lowered = &config["workspaces"]["practice"]["sources"]["mail"];
    assert_eq!(lowered["db"], mail.db);
    assert_eq!(lowered["query"], mail.query);
    assert_eq!(
        config["workspaces"]["practice"]["entities"][0],
        "collection:sqlite:mail"
    );
    // Definitions travel with the resolved program; the declaration is gone.
    assert!(resolved.source_kinds.contains_key("google-mail"));
}

#[test]
fn field_values_become_escaped_sql_literals() {
    let kind = r#"source kind k {
  needs who, n, flag, names
  database "/d.db"
  query "SELECT * FROM t WHERE who = {who} AND n = {n} AND f = {flag} AND name IN ({names})"
  id "id" when "t" unit s what "w"
}"#;
    let workspace = r#"workspace "w" {
  source k "s" {
    who "o'neil \\ '); DROP TABLE t; --"
    n 42
    flag true
    names { "a'b" "c" }
  }
}"#;
    let resolved = resolve_text(&[kind, workspace], &environment()).unwrap();
    assert_eq!(
        sqlite(&resolved, "w", "s").query,
        r"SELECT * FROM t WHERE who = 'o''neil \ ''); DROP TABLE t; --' AND n = 42 AND f = 1 AND name IN ('a''b', 'c')"
    );

    let nul = r#"workspace "w" { source k "s" { who "a\u0000b" n 1 flag false names { "x" } } }"#;
    assert!(error(&[kind, nul], &environment()).contains("NUL"));
}

#[test]
fn database_expands_placeholders_and_tilde_and_may_be_overridden() {
    let kind = r#"source kind notes {
  needs account
  database "~/Library/{account}/{workspace}.sqlite"
  query "SELECT 1 AS id, 1 AS t, 'x' AS w"
  id "id" when "t" unit s what "w"
}"#;
    let declared = r#"workspace "w" {
  source notes "a" { account "me" }
  source notes "b" { account "me" database "~/elsewhere.sqlite" }
}"#;
    let resolved = resolve_text(&[kind, declared], &environment()).unwrap();
    assert_eq!(sqlite(&resolved, "w", "a").db, "/home/demo/Library/me/w.sqlite");
    assert_eq!(sqlite(&resolved, "w", "b").db, "/home/demo/elsewhere.sqlite");
}

#[test]
fn builtin_kinds_come_from_the_host_and_programs_may_shadow_them() {
    let mut environment = environment();
    environment.register_kinds(MAIL_KIND).unwrap();
    // Registering the same definition twice is harmless; a different one is not.
    environment.register_kinds(MAIL_KIND).unwrap();
    let changed = MAIL_KIND.replace("who \"sender\"", "who \"from\"");
    assert!(
        format!("{:#}", environment.register_kinds(&changed).unwrap_err())
            .contains("conflicting built-in source kind google-mail")
    );
    assert!(
        format!(
            "{:#}",
            environment.register_kinds(MAIL_WORKSPACE).unwrap_err()
        )
        .contains("only source kind definitions")
    );

    let resolved = resolve_text(&[MAIL_WORKSPACE], &environment).unwrap();
    assert_eq!(sqlite(&resolved, "practice", "mail").who, SqliteWho::Columns { columns: vec!["sender".into()] });
    // A program's own definition takes precedence, as program profiles do.
    let resolved = resolve_text(&[&changed, MAIL_WORKSPACE], &environment).unwrap();
    assert_eq!(sqlite(&resolved, "practice", "mail").who, SqliteWho::Columns { columns: vec!["from".into()] });
    // The plain resolver knows no built-ins and keeps the old error.
    let message = format!(
        "{:#}",
        resolve(vec![parse(MAIL_WORKSPACE).unwrap()], Path::new("/home/demo")).unwrap_err()
    );
    assert!(message.contains("has no source kind google-mail"), "{message}");
    assert!(message.contains("must lower it before resolution"), "{message}");
}

#[test]
fn definitions_share_one_namespace_across_files() {
    // Identical definitions may repeat across files; conflicting ones may not.
    resolve_text(&[MAIL_KIND, MAIL_KIND, MAIL_WORKSPACE], &environment()).unwrap();
    let changed = MAIL_KIND.replace("unit ms", "unit s");
    assert!(
        error(&[MAIL_KIND, &changed, MAIL_WORKSPACE], &environment())
            .contains("conflicting source kind google-mail across config files")
    );
    let twice = format!("{MAIL_KIND}{MAIL_KIND}");
    assert!(format!("{:#}", parse(&twice).unwrap_err()).contains("duplicate source kind google-mail"));
}

#[test]
fn host_lowering_still_runs_before_resolution() {
    let mut program = parse(MAIL_WORKSPACE).unwrap();
    program
        .lower_host_sources(|_, host| {
            let mut kinds = Environment::new("/home/demo").with_enzyme_home("/srv");
            kinds.register_kinds(MAIL_KIND)?;
            Ok(Some(Source::Sqlite(kinds.builtin_kinds["google-mail"].expand(
                host,
                "practice",
                &kinds,
            )?)))
        })
        .unwrap();
    let resolved = resolve(vec![program], Path::new("/home/demo")).unwrap();
    assert_eq!(sqlite(&resolved, "practice", "mail").db, "/srv/workspaces/practice/ledger.db");
}

#[test]
fn declaration_errors_name_the_kind_and_its_fields() {
    let env = environment();
    let missing = r#"workspace "w" { source google-mail "mail" { query "x" } }"#;
    let message = error(&[MAIL_KIND, missing], &env);
    assert!(
        message.contains("source google-mail \"mail\" needs field \"account\"")
            && message.contains("needs account and accepts query, backfill days"),
        "{message}"
    );

    let unknown = r#"workspace "w" { source google-mail "mail" { account "a" acount "b" } }"#;
    let message = error(&[MAIL_KIND, unknown], &env);
    assert!(message.contains("unknown field \"acount\""), "{message}");

    let unknown_kind = r#"workspace "w" { source google-mial "mail" { account "a" } }"#;
    let message = error(&[MAIL_KIND, unknown_kind], &env);
    assert!(message.contains("has no source kind google-mial"), "{message}");

    let database = r#"workspace "w" { source google-mail "mail" { account "a" database 3 } }"#;
    assert!(error(&[MAIL_KIND, database], &env).contains("database must be quoted text"));

    // {home} needs a host that knows its Enzyme home.
    let message = error(&[MAIL_KIND, MAIL_WORKSPACE], &Environment::new("/home/demo"));
    assert!(message.contains("supplies no Enzyme home"), "{message}");
}

#[test]
fn definition_errors_are_reported_at_parse_time() {
    let kind = |body: &str| {
        format!(
            "source kind k {{\n{body}\n  id \"id\" when \"t\" unit s what \"w\"\n}}"
        )
    };
    let parse_error = |text: String| format!("{:#}", parse(&text).unwrap_err());
    let cases = [
        (
            kind(r#"database "/d" query "SELECT {acount}""#),
            "placeholder {acount} for an undeclared field",
        ),
        (
            kind(r#"database "/{acount}" query "SELECT 1""#),
            "placeholder {acount} for an undeclared field",
        ),
        (
            kind("accepts query\n database \"/d\" query \"SELECT {query}\""),
            "\"query\" is an accepts field",
        ),
        (
            kind("needs account\n database \"/d\" query \"SELECT '{account}'\""),
            "inside a quoted SQL string",
        ),
        (
            kind("needs account\n accepts account\n database \"/d\" query \"SELECT 1\""),
            "declares field \"account\" more than once",
        ),
        (
            kind("needs home\n database \"/d\" query \"SELECT 1\""),
            "cannot declare field \"home\"",
        ),
        (
            kind("needs a\n needs b\n database \"/d\" query \"SELECT 1\""),
            "setting is repeated",
        ),
        (kind(r#"query "SELECT 1""#), "source kind k \"k\" needs database"),
        (kind(r#"database "/d" query "SELECT 1" path "x""#), "in source kind k"),
        (
            "source kind sqlite { }".to_string(),
            "source kind sqlite is reserved",
        ),
        (
            r#"source markdown "x" { path "/x" }"#.to_string(),
            "sources belong inside a workspace",
        ),
        (
            r#"workspace "w" { source kind k { } }"#.to_string(),
            "belong at the top level",
        ),
    ];
    for (text, expected) in cases {
        let message = parse_error(text.clone());
        assert!(message.contains(expected), "{text}\n=> {message}");
    }
}
