# enzyme-spec

Parser, resolver, and renderer for the readable `.enzyme` workspace language.
It is pure: parsing, resolution, lowering to the engine's configuration value,
canonical rendering, and agent instructions, with no engine or I/O dependency
beyond reading `.enzyme` files from a directory.

`.enzyme` files describe what an Enzyme workspace reads (sources and
exclusions), what it learns questions about (readings, profiles, budgets,
sampling), and how agents use it (`when asked`, `remember in`, create-note
policies). This crate is the reference parser used by the Enzyme engine and by
hosts such as [Margins](https://github.com/byenzyme/margins).

## Example

```enzyme
workspace "practice" {
  source markdown "notes" { path "~/notes" }
  source sqlite "chat" {
    database "~/chat.db"
    query """SELECT id, sender, sent_ms, body FROM messages"""
    id "id"
    who "sender"
    when "sent_ms" unit ms
    what "body"
  }

  remember in folder "inbox" create note
  leave out folders { "archive" }

  question budget 12
  sample by time
  learn questions from folder "people" including linked pages about relationships
  learn questions from source "chat" including who links
}
```

```rust
let program = enzyme_spec::parse(text)?;
let resolved = enzyme_spec::resolve(vec![program], &user_home)?;
let canonical = enzyme_spec::render_program(&resolved);
```

## Embedding hosts

A host that defines its own source kinds (for example `source google-mail
"mail" { account "me@example.com" }`) parses them as `Source::Host`, then
replaces each one with a native Markdown, SQLite, or Are.na source before
resolution:

```rust
let mut program = enzyme_spec::parse(text)?;
program.lower_host_sources(|workspace, host| {
    Ok(match host.kind.as_str() {
        "google-mail" => Some(my_mail_source(workspace, host)?),
        _ => None, // left in place; resolve() rejects it
    })
})?;
let resolved = enzyme_spec::resolve(vec![program], &user_home)?;
```

Create-note policies (`remember in folder "inbox" create note`) are exposed as
`Workspace::note_policies`; `Workspace::note_policy_source` names the Markdown
source a policy writes into, and resolution fills `NotePolicy::root`.

## Plan and apply

`enzyme_spec::plan` changes a program file only through a reviewed plan.
A plan (`enzyme.plan.v1` JSON) names the workspace and the file that holds it,
the file's current revision (SHA-256 of its bytes, or `"absent"`), the desired
text and its digest, a unified diff, a statement-level summary (sources,
readings, exclusions, settings, profiles), and a `plan_id` over all of them.

```rust
let store = enzyme_spec::plan::ConfigStore::new(home.join("configs"), &user_home);
let plan = store.plan("practice", &desired_text)?; // review plan.changes / plan.diff
let receipt = store.apply(&plan)?;
```

`apply` writes exactly the desired text, atomically, under a lock, and only
while the file is still at the planned revision and the plan is exactly what
planning produces now; otherwise it returns `ApplyError::Stale` or
`ApplyError::Altered`. The write is journaled first, so an interrupted apply
is completed by the next caller, and applying an applied plan again returns
its receipt with `replayed: true`. A new workspace is written to
`<name>.enzyme`. State lives in `configs/.enzyme-apply/`, which
`load_directory` ignores. Hosts that lower their own source kinds pass a
validator with `ConfigStore::with_validator`.

## License

Apache-2.0. The Enzyme engine that consumes these programs is distributed
separately.
