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

## Readings and automatic selection

Without readings, the engine chooses what to learn questions about on its own
(automatic selection). Once a vault or workspace declares readings, they are
the complete set. `learn questions automatically` keeps automatic selection
running alongside them:

```enzyme
workspace "meetings" {
  source markdown "notes" { path "~/notes" }
  learn questions from folder "Meetings"
  learn questions from folder "People" including linked pages
  learn questions automatically up to 10
  leave out folders { "Templates" }
}
```

The engine treats the declared readings as automatic selection's first picks:
each entity a reading names counts toward the engine's automatic limit (20 in
Enzyme; `settings { selection limit N }` changes it), and the documents of
everything the readings select count as already covered, so automatic picks
favor what the readings miss. `up to N` caps the automatic picks further; it
never raises the limit. A reading always takes precedence for its entity (the
entity is learned once, with the reading's settings), and a reading's own cap
holds: automatic selection never adds a match of a declared `matching` pattern,
a linked page of a declared folder, or a who link of a source read `including
who links`. Leave-outs apply to automatic picks, and hosts never write
automatic picks back into the program. The
statement renders after the readings, and without readings it simply allows
`up to N` to cap ordinary automatic selection. `learn questions automatically
select …` is an error: automatic selection ranks by coverage, not by frequency
or recency.

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

- `{workspace}` is the workspace name, `{source}` the declaration's name
  (`"mail"` above), `{home}` the Enzyme home supplied by the host, and
  `{field}` the value of a `needs` field. No other placeholder is allowed, and
  an `accepts` field cannot be substituted. `{source}` lets a template build
  readable document refs without knowing its declaration:
  `'sqlite:' || {source} || '/' || id AS ref` with `document ref "ref"`.
  The Enzyme engine requires refs inside `sqlite:<key>/`, where `<key>` is
  the name with bytes other than ASCII letters, digits, `-`, `_` and `.`
  percent-escaped, so in `query` `{source}` is that key: `"My Mail"` becomes
  `'My%20Mail'` and `"Café"` becomes `'Caf%C3%A9'` (names made only of those
  characters are unchanged). In `database` it stays the declared name. A source name is a name,
  not a path: `.`, `..`, `/`, `\` and NUL are rejected. `source` is reserved,
  so a kind can no longer declare a field named `source`.
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

## One directory, many workspaces

`resolve` and `load_directory` are strict: any invalid declaration fails the
whole namespace. A host that keeps several workspaces in one config directory
uses `resolve_namespace_in` / `load_namespace_in` instead, which resolve each
workspace and vault on its own and return a `Namespace`: the program of
everything that resolved, plus `problems`, each scoped to the declaration it
breaks (`Scope::Workspace`, `Scope::Vault`, a `Scope::File` that does not
parse, or a `Scope::Profile`/`Scope::SourceKind` defined differently in two
files). `Namespace::workspace_problems(name)` is what makes one workspace
unusable: its own problems, and unparsable files when it resolved nowhere.

- Workspaces are addressed by name, so two workspaces may read the same
  Markdown folder. Only two `vault "<path>"` declarations of one path
  conflict. `Program::keyed_vaults` gives each resolved vault its runtime key:
  `workspace:<name>`, a `vault`'s path, and the path of a lone-Markdown
  workspace while nothing else claims it. `Namespace::lone_markdown_paths`
  lists every lone-Markdown workspace's folder as declared, including ones
  that failed to resolve, so a host can tell a folder is shared even while one
  of its workspaces is invalid.
- A profile or source kind defined differently in two files fails exactly
  the workspaces and vaults that use it.
- What every declaration inherits is still one namespace: repeated
  `settings` fields, global learning, or global retrieval are errors for all.

`resolve_with_in(others, candidate, environment)` checks one file against the
rest: it fails on the candidate's own problems and on problems it causes in
other files, but tolerates problems the other files already had.

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
is completed by the next caller; a journal whose file was edited by hand in
between blocks only that file, and `ConfigStore::discard_unrecoverable` drops
it. Applying an applied plan again, while the file is still at its result,
returns its receipt with `replayed: true`. A new workspace is written to
`<name>.enzyme`. The lock is always `configs/.enzyme-apply/lock`; journals and
receipts live there too unless `with_state_dir` moves them. `load_directory`
ignores that directory. Writes replace the file by rename, keeping its
permissions and writing through a symlink; a hard link keeps the old text.
Moving a workspace between files, renaming, or removing it is out of scope.
Planning validates the desired program with `resolve_with`, so another
config file that is invalid (or does not parse) never blocks editing this
one; a desired program that breaks another file is refused. A validator
receives the other programs followed by the candidate, last. Hosts with
built-in kinds or an Enzyme home pass `plan::resolver_in(environment)`, and
hosts that lower their own source kinds pass any validator, with
`ConfigStore::with_validator`.

## License

Apache-2.0. The Enzyme engine that consumes these programs is distributed
separately.
