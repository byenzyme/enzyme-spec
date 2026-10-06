//! Program plan/apply: summaries, stale and altered plans, replay, recovery.
use enzyme_spec::plan::*;
use std::path::Path;

const BASE: &str = r#"workspace "practice" {
  source markdown "notes" { path "/srv/notes" }

  leave out folders { "archive" }
  learn questions from folder "notes/people" about relationships
}
"#;

const DESIRED: &str = r#"workspace "practice" {
  source markdown "notes" { path "/srv/notes" }
  source sqlite "chat" {
    database "/srv/chat.db"
    query "SELECT id, sender, sent_ms, body FROM messages"
    id "id"
    who "sender"
    when "sent_ms" unit ms
    what "body"
  }

  leave out folders { "templates" }
  leave out tags { "private" }
  question budget 12
  learn questions from folder "notes/people" about decisions
  learn questions from source "chat"
}
"#;

fn open(dir: &Path) -> ConfigStore {
    ConfigStore::new(dir.join("configs"), "/home/demo")
}

fn write(dir: &Path, name: &str, text: &str) {
    std::fs::create_dir_all(dir.join("configs")).unwrap();
    std::fs::write(dir.join("configs").join(name), text).unwrap();
}

fn read(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join("configs").join(name)).unwrap()
}

fn kind(error: anyhow::Error) -> ApplyError {
    error
        .downcast_ref::<ApplyError>()
        .unwrap_or_else(|| panic!("not an ApplyError: {error:#}"))
        .clone()
}

fn summaries(plan: &Plan) -> Vec<&str> {
    plan.changes.iter().map(|c| c.summary.as_str()).collect()
}

#[test]
fn plan_summarizes_statement_changes_and_apply_writes_exactly_desired() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "practice.enzyme", BASE);
    write(
        tmp.path(),
        "profiles.enzyme",
        "profile clients {\n  seek \"what each client needs\"\n  notice { \"commitments\" }\n}\n",
    );
    let store = open(tmp.path());
    let plan = store.plan("practice", DESIRED).unwrap();
    assert_eq!(plan.schema, PLAN_SCHEMA);
    assert_eq!(plan.target, "practice.enzyme");
    assert_eq!(plan.base_revision, revision(Some(BASE)));
    assert_eq!(plan.desired_sha256, sha256(DESIRED.as_bytes()));
    assert!(plan.diff.contains("--- a/practice.enzyme"));
    assert!(plan.diff.contains("+  leave out tags { \"private\" }"));
    assert_eq!(
        summaries(&plan),
        [
            "Add sqlite source \"chat\"",
            "Change reading of folder:notes/people: folder:notes/people about decisions",
            "Learn questions from source:chat",
            "Stop leaving out folder \"archive\"",
            "Leave out folder \"templates\"",
            "Leave out tag \"private\"",
            "Change learning in workspace \"practice\"",
        ],
        "{:#?}",
        plan.changes
    );
    // The plan is plain JSON and survives a round trip.
    let plan: Plan = serde_json::from_str(&serde_json::to_string(&plan).unwrap()).unwrap();
    let receipt = store.apply(&plan).unwrap();
    assert!(!receipt.replayed);
    assert_eq!(receipt.before_revision, plan.base_revision);
    assert_eq!(receipt.after_revision, plan.desired_sha256);
    assert_eq!(read(tmp.path(), "practice.enzyme"), DESIRED);
    assert!(!store.state().join("journal.json").exists());
    assert!(
        store
            .state()
            .join("receipts")
            .join(format!("{}.json", plan.plan_id))
            .exists()
    );
    // State never joins the namespace.
    assert!(enzyme_spec::load_directory(store.configs(), Path::new("/home/demo")).is_ok());
}

#[test]
fn a_new_workspace_gets_its_own_file() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "other.enzyme",
        "workspace \"other\" {\n  source markdown \"n\" { path \"/srv/other\" }\n}\n",
    );
    let store = open(tmp.path());
    let plan = store.plan("practice", BASE).unwrap();
    assert_eq!(plan.target, "practice.enzyme");
    assert_eq!(plan.base_revision, ABSENT);
    assert_eq!(plan.changes[0].summary, "Create workspace \"practice\"");
    assert_eq!(plan.changes[0].action, Action::Added);
    store.apply(&plan).unwrap();
    assert_eq!(read(tmp.path(), "practice.enzyme"), BASE);
    // Planning again finds the same file.
    let next = store.plan("practice", DESIRED).unwrap();
    assert_eq!(next.target, "practice.enzyme");
    assert_eq!(next.base_revision, revision(Some(BASE)));
    assert_eq!(default_target("My Notes/2"), "My-Notes-2.enzyme");
}

#[test]
fn an_existing_workspace_is_planned_in_the_file_that_declares_it() {
    let tmp = tempfile::tempdir().unwrap();
    let shared = format!("settings {{\n  generation hosted\n}}\n\n{BASE}");
    write(tmp.path(), "all.enzyme", &shared);
    let plan = open(tmp.path())
        .plan("practice", &shared.replace("hosted", "local"))
        .unwrap();
    assert_eq!(plan.target, "all.enzyme");
    assert_eq!(summaries(&plan), ["Change generation"]);
}

#[test]
fn invalid_desired_programs_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "practice.enzyme", BASE);
    write(
        tmp.path(),
        "other.enzyme",
        "workspace \"other\" {\n  source markdown \"n\" { path \"/srv/other\" }\n}\n",
    );
    let store = open(tmp.path());
    let error = |desired: &str, workspace: &str| {
        format!("{:#}", store.plan(workspace, desired).unwrap_err())
    };
    assert!(error("workspace {", "practice").contains("does not parse"));
    assert!(error(&BASE.replace("practice", "renamed"), "practice").contains("does not declare"));
    assert!(
        error(&BASE.replace("relationships", "nosuch"), "practice").contains("unknown profile")
    );
    // A workspace already declared in another file cannot be planned into this one.
    let both = format!(
        "{BASE}\nworkspace \"other\" {{\n  source markdown \"n\" {{ path \"/srv/o\" }}\n}}\n"
    );
    assert!(error(&both, "practice").contains("declared in another config file"));
    // A file of the default name that holds something else is not taken over.
    write(
        tmp.path(),
        "fresh.enzyme",
        "profile fresh {\n  seek \"x\"\n  notice { \"y\" }\n}\n",
    );
    assert!(
        error(
            "workspace \"fresh\" {\n  source markdown \"n\" { path \"/srv/f\" }\n}\n",
            "fresh"
        )
        .contains("does not declare workspace")
    );
}

#[test]
fn a_stale_plan_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "practice.enzyme", BASE);
    let store = open(tmp.path());
    let plan = store.plan("practice", DESIRED).unwrap();
    let edited = BASE.replace("archive", "old");
    write(tmp.path(), "practice.enzyme", &edited);
    match kind(store.apply(&plan).unwrap_err()) {
        ApplyError::Stale { expected, actual } => {
            assert_eq!(expected, revision(Some(BASE)));
            assert_eq!(actual, revision(Some(&edited)));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(read(tmp.path(), "practice.enzyme"), edited);

    // A plan to create a file that has since appeared is stale too.
    let plan = store
        .plan(
            "new",
            "workspace \"new\" {\n  source markdown \"n\" { path \"/srv/n\" }\n}\n",
        )
        .unwrap();
    write(
        tmp.path(),
        "new.enzyme",
        "workspace \"new\" {\n  source markdown \"n\" { path \"/srv/x\" }\n}\n",
    );
    assert!(matches!(
        kind(store.apply(&plan).unwrap_err()),
        ApplyError::Stale { .. }
    ));
}

#[test]
fn altered_plans_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "practice.enzyme", BASE);
    write(
        tmp.path(),
        "other.enzyme",
        "workspace \"other\" {\n  source markdown \"n\" { path \"/srv/other\" }\n}\n",
    );
    let store = open(tmp.path());
    let plan = store.plan("practice", DESIRED).unwrap();
    let altered = |edit: &dyn Fn(&mut Plan)| {
        let mut plan = plan.clone();
        edit(&mut plan);
        store.apply(&plan).unwrap_err()
    };
    // Desired text without its digest.
    assert!(matches!(
        kind(altered(&|p| p.desired.push_str("\n// sneaky\n"))),
        ApplyError::Altered(_)
    ));
    // Desired text with a matching digest but a hidden change.
    assert!(matches!(
        kind(altered(&|p| {
            p.desired = p.desired.replace("templates", "everything");
            p.desired_sha256 = sha256(p.desired.as_bytes());
        })),
        ApplyError::Altered(_)
    ));
    // A summary or diff that does not describe the change.
    assert!(matches!(
        kind(altered(&|p| p.changes.clear())),
        ApplyError::Altered(_)
    ));
    assert!(matches!(
        kind(altered(&|p| p.diff.clear())),
        ApplyError::Altered(_)
    ));
    // Another target, a path, or a forged plan_id.
    assert!(matches!(
        kind(altered(&|p| p.target = "other.enzyme".into())),
        ApplyError::Altered(_)
    ));
    assert!(
        format!("{:#}", altered(&|p| p.target = "../escape.enzyme".into()))
            .contains("plain .enzyme file name")
    );
    assert!(matches!(
        kind(altered(&|p| p.plan_id = "../../x".into())),
        ApplyError::Altered(_)
    ));
    assert!(matches!(
        kind(altered(&|p| p.schema = "enzyme.plan.v0".into())),
        ApplyError::Unsupported(_)
    ));
    assert_eq!(read(tmp.path(), "practice.enzyme"), BASE);
    assert!(!store.state().join("journal.json").exists());
}

#[test]
fn applying_twice_replays_the_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "practice.enzyme", BASE);
    let store = open(tmp.path());
    let plan = store.plan("practice", DESIRED).unwrap();
    let first = store.apply(&plan).unwrap();
    // A later edit does not make the replay stale: the plan was applied.
    let later = DESIRED.replace("templates", "drafts");
    write(tmp.path(), "practice.enzyme", &later);
    let second = store.apply(&plan).unwrap();
    assert!(second.replayed);
    assert_eq!(
        Receipt {
            replayed: false,
            ..second
        },
        first
    );
    assert_eq!(read(tmp.path(), "practice.enzyme"), later);
    // The same plan_id with other contents is not a replay.
    let mut forged = plan.clone();
    forged.diff.push(' ');
    assert!(matches!(
        kind(store.apply(&forged).unwrap_err()),
        ApplyError::Altered(_)
    ));
}

#[test]
fn an_interrupted_apply_is_completed_by_the_next_caller() {
    // Interrupted after the journal, before the target was written.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "practice.enzyme", BASE);
    let store = open(tmp.path());
    let plan = store.plan("practice", DESIRED).unwrap();
    let journal = store.prepare(&plan).unwrap();
    assert_eq!(read(tmp.path(), "practice.enzyme"), BASE);
    assert!(store.state().join("journal.json").exists());
    // Planning first completes it, so its base is the recovered file.
    let next = store.plan("practice", BASE).unwrap();
    assert_eq!(next.base_revision, plan.desired_sha256);
    assert_eq!(read(tmp.path(), "practice.enzyme"), DESIRED);
    assert!(!store.state().join("journal.json").exists());
    let replay = store.apply(&plan).unwrap();
    assert!(replay.replayed);
    assert_eq!(
        Receipt {
            replayed: false,
            ..replay
        },
        journal.receipt
    );

    // Interrupted after the target was written, before the receipt.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "practice.enzyme", BASE);
    let store = open(tmp.path());
    let plan = store.plan("practice", DESIRED).unwrap();
    store.prepare(&plan).unwrap();
    write(tmp.path(), "practice.enzyme", DESIRED);
    let recovered = store.recover().unwrap().unwrap();
    assert_eq!(recovered.plan_id, plan.plan_id);
    assert!(store.apply(&plan).unwrap().replayed);

    // Someone edited the file in between: refuse to guess.
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "practice.enzyme", BASE);
    let store = open(tmp.path());
    let plan = store.plan("practice", DESIRED).unwrap();
    store.prepare(&plan).unwrap();
    write(tmp.path(), "practice.enzyme", "workspace \"practice\" {}\n");
    assert!(matches!(
        kind(store.recover().unwrap_err()),
        ApplyError::Unrecoverable(_)
    ));
    assert!(matches!(
        kind(store.apply(&plan).unwrap_err()),
        ApplyError::Unrecoverable(_)
    ));
}

#[test]
fn layout_only_and_unparseable_current_files_are_explained() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "practice.enzyme", BASE);
    let store = open(tmp.path());
    let commented = format!("// my notes\n{BASE}");
    let plan = store.plan("practice", &commented).unwrap();
    assert_eq!(plan.changes.len(), 1);
    assert_eq!(plan.changes[0].statement, Statement::Layout);
    let same = store.plan("practice", BASE).unwrap();
    assert!(same.is_noop() && same.changes.is_empty() && same.diff.is_empty());

    write(
        tmp.path(),
        "practice.enzyme",
        "workspace \"practice\" {\n  oops\n",
    );
    let plan = store.plan("practice", BASE).unwrap();
    assert_eq!(plan.target, "practice.enzyme");
    assert_eq!(plan.changes[0].statement, Statement::Program);
    store.apply(&plan).unwrap();
    assert_eq!(read(tmp.path(), "practice.enzyme"), BASE);
}

#[test]
fn hosts_can_validate_with_their_own_lowering() {
    let tmp = tempfile::tempdir().unwrap();
    let hosted = "workspace \"practice\" {\n  source google-mail \"mail\" { account \"me@example.com\" }\n}\n";
    let plain = open(tmp.path());
    assert!(format!("{:#}", plain.plan("practice", hosted).unwrap_err()).contains("host source"));
    let lowering = open(tmp.path()).with_validator(Box::new(|mut programs| {
        for program in &mut programs {
            program.lower_host_sources(|_, host| {
                Ok(Some(enzyme_spec::Source::Markdown(
                    enzyme_spec::MarkdownSource {
                        name: host.name.clone(),
                        path: "/srv/mail".into(),
                    },
                )))
            })?;
        }
        enzyme_spec::resolve(programs, Path::new("/home/demo")).map(drop)
    }));
    let plan = lowering.plan("practice", hosted).unwrap();
    assert_eq!(summaries(&plan)[1], "Add google-mail source \"mail\"");
    lowering.apply(&plan).unwrap();
}

#[test]
fn concurrent_applies_from_one_base_serialize() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "practice.enzyme", BASE);
    let plans: Vec<Plan> = ["a", "b", "c", "d"]
        .iter()
        .map(|tag| {
            open(tmp.path())
                .plan("practice", &BASE.replace("archive", tag))
                .unwrap()
        })
        .collect();
    let results: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = plans
            .iter()
            .map(|plan| scope.spawn(|| open(tmp.path()).apply(plan)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut applied = Vec::new();
    for result in results {
        match result {
            Ok(receipt) => applied.push(receipt),
            Err(error) => assert!(matches!(kind(error), ApplyError::Stale { .. })),
        }
    }
    assert_eq!(applied.len(), 1);
    assert_eq!(
        revision(Some(&read(tmp.path(), "practice.enzyme"))),
        applied[0].after_revision
    );
}
