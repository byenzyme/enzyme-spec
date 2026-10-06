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

## Source kinds

A `source kind` turns a source into SQL once, so declarations stay short:

```enzyme
source kind google-mail {
  needs account                       // required fields of a declaration
  accepts query, backfill days        // host-only fields; ignored here, typos still error
  database "{home}/workspaces/{workspace}/ledger.db"
  query """SELECT id, sender, sent_ms, body FROM mail WHERE account = {account}"""
  id "id"  who "sender"  when "sent_ms" unit ms  what "body"
}

workspace "practice" {
  source google-mail "mail" { account "me@example.com" backfill days 365 }
  learn questions from source "mail"
}
```

`resolve` expands each declaration to the native SQLite source the template
describes, keeping the declared name:

- `{workspace}` is the workspace name, `{home}` the Enzyme home supplied by the
  host, and `{field}` the value of a `needs` field. No other placeholder is
  allowed, and an `accepts` field cannot be substituted.
- In `query`, a placeholder stands for a whole SQL value: text becomes a
  single-quoted literal with quotes doubled, integers stay numbers, `true`/`false`
  become `1`/`0`, and a list becomes a parenthesized list of literals, so write
  `IN {tags}` (an empty list is an error).
  Write placeholders bare (`= {account}`, not `= '{account}'`); one inside a
  quoted string or identifier is an error, and comments are never substituted.
  Values containing NUL are rejected.
- In `database`, placeholders are path text, then `~` expands. Any declaration
  may give `database "…"` to replace the template's path; the override fills
  the same placeholders, so `database` cannot be a `needs` or `accepts` field.
  Workspace names, which fill `{workspace}`, must not contain `/`, `\`, or NUL,
  or be `.`/`..`.
- A declaration missing a `needs` field, or giving a field that is neither
  needed nor accepted, is an error that lists the kind's fields.

Definitions share the program namespace like profiles: identical definitions
may repeat across files, conflicting ones are errors. A host can ship built-in
kinds; a program's own definition of the same name takes precedence:

```rust
let mut environment = enzyme_spec::Environment::new(&user_home)
    .with_enzyme_home(&enzyme_home);
environment.register_kinds(include_str!("builtin-sources.enzyme"))?;
let resolved = enzyme_spec::resolve_in(programs, &environment)?;
```

`resolve(programs, user_home)` is `resolve_in` with no Enzyme home and no
built-in kinds.

## Embedding hosts

A declaration whose kind has no definition parses as `Source::Host`, and
`resolve` rejects it naming the missing kind. A host that still lowers its
own kinds in code replaces each one with a native Markdown, SQLite, or Are.na
source before resolution:

```rust
let mut program = enzyme_spec::parse(text)?;
program.lower_host_sources(|workspace, host| {
    Ok(match host.kind.as_str() {
        "google-mail" => Some(my_mail_source(workspace, host)?),
        _ => None, // left in place; resolve() expands or rejects it
    })
})?;
let resolved = enzyme_spec::resolve(vec![program], &user_home)?;
```

Create-note policies (`remember in folder "inbox" create note`) are exposed as
`Workspace::note_policies`; `Workspace::note_policy_source` names the Markdown
source a policy writes into, and resolution fills `NotePolicy::root`.

## License

Apache-2.0. The Enzyme engine that consumes these programs is distributed
separately.
