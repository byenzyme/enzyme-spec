//! `source kind` definitions: named SQLite templates that turn a declaration
//! such as `source google-mail "mail" { account "me@example.com" }` into a
//! native SQLite source during [`crate::resolve`].
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::{HostSource, HostValue, Program, SqliteSource};

/// `source kind <name> { needs …  accepts …  <SQLite source fields> }`.
///
/// `needs` names the fields every declaration must give; their values fill
/// `{field}` placeholders. `accepts` names fields the host reads and the engine
/// ignores, so a misspelled field is still an error. Any declaration may also
/// give `database "…"` to replace the template's database path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceKind {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepts: Vec<String>,
    /// The SQLite source every declaration expands to. `db` and `query` may
    /// hold `{workspace}`, `{home}`, and `{<needs field>}` placeholders; `name`
    /// is the kind name and is replaced by the declaration's name.
    pub template: SqliteSource,
}

/// Source kind words the language interprets itself.
pub(crate) const NATIVE_KINDS: &[&str] = &["markdown", "sqlite", "arena", "kind"];

/// Placeholders every template may use besides its `needs` fields.
const WORKSPACE: &str = "workspace";
const HOME: &str = "home";

impl SourceKind {
    /// Check the definition on its own: names, field lists, and that every
    /// placeholder in `database` and `query` is declared.
    pub fn validate(&self) -> Result<()> {
        let kind = &self.name;
        ensure!(
            !NATIVE_KINDS.contains(&kind.as_str()),
            "source kind {kind} is reserved by the language"
        );
        ensure!(
            kind.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && kind
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "source kind name {kind:?} must be a word such as google-mail"
        );
        let mut seen = std::collections::BTreeSet::new();
        for field in self.needs.iter().chain(&self.accepts) {
            ensure!(
                is_field_name(field),
                "source kind {kind} field {field:?} must be one or more words"
            );
            ensure!(
                field != WORKSPACE && field != HOME,
                "source kind {kind} cannot declare field {field:?}; {{{field}}} is always supplied"
            );
            ensure!(
                field != "database",
                "source kind {kind} cannot declare field \"database\"; every declaration may already override database"
            );
            ensure!(
                seen.insert(field.as_str()),
                "source kind {kind} declares field {field:?} more than once"
            );
        }
        let declared = |name: &str| {
            name == WORKSPACE || name == HOME || self.needs.iter().any(|need| need == name)
        };
        let undeclared = |name: &str| self.undeclared_placeholder(name);
        fill_path(&self.template.db, &mut |name| {
            if declared(name) {
                Ok(String::new())
            } else {
                Err(undeclared(name))
            }
        })
        .context("database")?;
        fill_query(&self.template.query, &mut |name| {
            declared(name).then(|| Ok("NULL".to_string()))
        })
        .map_err(|error| match error {
            FillError::Unknown(name) => undeclared(&name),
            FillError::Other(error) => error,
        })
        .context("query")?;
        Ok(())
    }

    fn undeclared_placeholder(&self, name: &str) -> anyhow::Error {
        let kind = &self.name;
        if self.accepts.iter().any(|field| field == name) {
            anyhow::anyhow!(
                "source kind {kind} uses placeholder {{{name}}}, but {name:?} is an accepts field; only needs fields, {{workspace}}, and {{home}} can be substituted"
            )
        } else {
            anyhow::anyhow!(
                "source kind {kind} uses placeholder {{{name}}} for an undeclared field; add `needs {name}` or use {{workspace}} or {{home}}"
            )
        }
    }

    /// Expand one declaration of this kind to its native SQLite source.
    pub fn expand(
        &self,
        host: &HostSource,
        workspace: &str,
        environment: &Environment,
    ) -> Result<SqliteSource> {
        let kind = &self.name;
        let source = &host.name;
        let mut database = None;
        for field in &host.fields {
            let key = field.key.as_str();
            if self.needs.iter().chain(&self.accepts).any(|f| f == key) {
                continue;
            }
            // validate() keeps `database` out of needs/accepts, so it is
            // always the override.
            if key == "database" {
                let HostValue::Text(path) = &field.value else {
                    bail!("source {kind} {source:?}: database must be quoted text");
                };
                database = Some(path.clone());
                continue;
            }
            bail!(
                "source {kind} {source:?} has unknown field {key:?}; source kind {kind} {}",
                self.describe_fields()
            );
        }
        for need in &self.needs {
            ensure!(
                host.field(need).is_some(),
                "source {kind} {source:?} needs field {need:?}; source kind {kind} {}",
                self.describe_fields()
            );
        }
        let value = |name: &str| -> Result<HostValue> {
            Ok(match name {
                WORKSPACE => HostValue::Text(workspace.to_string()),
                HOME => HostValue::Text(
                    environment
                        .enzyme_home
                        .as_ref()
                        .with_context(|| {
                            format!(
                                "source kind {kind} uses {{home}}, but this host supplies no Enzyme home"
                            )
                        })?
                        .to_string_lossy()
                        .into_owned(),
                ),
                _ => host
                    .field(name)
                    .cloned()
                    .with_context(|| format!("source kind {kind} has no field {name:?}"))?,
            })
        };
        // The template's path and a declaration's override fill the same way.
        let path = database.as_deref().unwrap_or(&self.template.db);
        let db = fill_path(path, &mut |name| {
            let declared =
                name == WORKSPACE || name == HOME || self.needs.iter().any(|need| need == name);
            if !declared {
                return Err(self.undeclared_placeholder(name));
            }
            match value(name)? {
                HostValue::Text(text) => Ok(text),
                HostValue::Integer(n) => Ok(n.to_string()),
                _ => bail!(
                    "source {kind} {source:?}: field {name:?} must be text or an integer to appear in the database path"
                ),
            }
        })
        .context("database")?;
        let query = fill_query(&self.template.query, &mut |name| {
            let declared =
                name == WORKSPACE || name == HOME || self.needs.iter().any(|need| need == name);
            declared.then(|| sql_value(&value(name)?))
        })
        .map_err(|error| match error {
            FillError::Unknown(name) => self.undeclared_placeholder(&name),
            FillError::Other(error) => error,
        })?;
        Ok(SqliteSource {
            name: host.name.clone(),
            db,
            query,
            ..self.template.clone()
        })
    }

    fn describe_fields(&self) -> String {
        let list = |fields: &[String]| {
            if fields.is_empty() {
                "nothing".to_string()
            } else {
                fields.join(", ")
            }
        };
        format!(
            "needs {} and accepts {} (plus database)",
            list(&self.needs),
            list(&self.accepts)
        )
    }
}

/// What resolution knows beyond the program text: the user's home for `~`,
/// the Enzyme home for `{home}`, and source kinds the host ships built in.
#[derive(Debug, Clone, Default)]
pub struct Environment {
    pub user_home: PathBuf,
    pub enzyme_home: Option<PathBuf>,
    /// Built-in kinds. A program's own definition of the same name takes
    /// precedence, as a program profile does over a built-in profile.
    pub builtin_kinds: BTreeMap<String, SourceKind>,
}

impl Environment {
    pub fn new(user_home: impl Into<PathBuf>) -> Self {
        Self {
            user_home: user_home.into(),
            ..Default::default()
        }
    }

    pub fn with_enzyme_home(mut self, enzyme_home: impl Into<PathBuf>) -> Self {
        self.enzyme_home = Some(enzyme_home.into());
        self
    }

    /// Register one built-in kind. Registering a different definition under
    /// an existing name is an error.
    pub fn register_kind(&mut self, kind: SourceKind) -> Result<()> {
        kind.validate()?;
        if let Some(existing) = self.builtin_kinds.get(&kind.name) {
            ensure!(
                *existing == kind,
                "conflicting built-in source kind {}",
                kind.name
            );
            return Ok(());
        }
        self.builtin_kinds.insert(kind.name.clone(), kind);
        Ok(())
    }

    /// Register every `source kind` in `.enzyme` text that defines nothing else.
    pub fn register_kinds(&mut self, text: &str) -> Result<()> {
        let program = crate::parse(text)?;
        let kinds = program.source_kinds.clone();
        ensure!(
            Program {
                source_kinds: BTreeMap::new(),
                ..program
            } == Program::default(),
            "built-in source kind text must contain only source kind definitions"
        );
        for kind in kinds.into_values() {
            self.register_kind(kind)?;
        }
        Ok(())
    }
}

pub(crate) fn is_field_name(name: &str) -> bool {
    !name.is_empty()
        && name.split(' ').all(|word| {
            word.chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && word
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

/// The placeholder starting at `chars[at] == '{'`: its name and the index just
/// past the closing brace, if the braces enclose a field name.
fn placeholder(chars: &[char], at: usize) -> Option<(String, usize)> {
    let close = chars[at + 1..].iter().position(|&c| c == '}')? + at + 1;
    let name: String = chars[at + 1..close].iter().collect();
    is_field_name(&name).then_some((name, close + 1))
}

/// Substitute placeholders in a database path as plain text.
fn fill_path(path: &str, value: &mut dyn FnMut(&str) -> Result<String>) -> Result<String> {
    let chars: Vec<char> = path.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some((name, next)) = (chars[i] == '{').then(|| placeholder(&chars, i)).flatten() {
            out.push_str(&value(&name)?);
            i = next;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    Ok(out)
}

enum FillError {
    Unknown(String),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for FillError {
    fn from(error: anyhow::Error) -> Self {
        Self::Other(error)
    }
}

/// Substitute bare placeholders in SQL with literals from `value`, which
/// returns `None` for a name that is not a placeholder of this kind.
///
/// Quoted strings and identifiers are copied unchanged, but naming a
/// placeholder inside one is an error: substitution only ever produces a
/// whole SQL literal, never raw text inside a quote. Comments are copied
/// unchanged and never substituted.
fn fill_query(
    query: &str,
    value: &mut dyn FnMut(&str) -> Option<Result<String>>,
) -> std::result::Result<String, FillError> {
    let chars: Vec<char> = query.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let close = match c {
            '\'' | '"' | '`' => Some(c),
            '[' => Some(']'),
            _ => None,
        };
        if let Some(close) = close {
            let start = i;
            i += 1;
            loop {
                if i >= chars.len() {
                    break;
                }
                if chars[i] == close {
                    // A doubled closing quote is an escaped quote, not the end.
                    if close != ']' && chars.get(i + 1) == Some(&close) {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                let quoted = (chars[i] == '{')
                    .then(|| placeholder(&chars, i))
                    .flatten()
                    .filter(|(name, _)| value(name).is_some());
                if let Some((name, _)) = quoted {
                    return Err(FillError::Other(anyhow::anyhow!(
                        "placeholder {{{name}}} is inside a quoted SQL string or identifier; write it bare, e.g. `= {{{name}}}`, and it is substituted as a quoted literal"
                    )));
                }
                i += 1;
            }
            out.extend(&chars[start..i]);
            continue;
        }
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            let end = chars[i..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(chars.len(), |p| i + p);
            out.extend(&chars[i..end]);
            i = end;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            let end = (i + 2..chars.len().saturating_sub(1))
                .find(|&j| chars[j] == '*' && chars[j + 1] == '/')
                .map_or(chars.len(), |j| j + 2);
            out.extend(&chars[i..end]);
            i = end;
            continue;
        }
        if let Some((name, next)) = (c == '{').then(|| placeholder(&chars, i)).flatten() {
            match value(&name) {
                Some(literal) => out.push_str(&literal?),
                None => return Err(FillError::Unknown(name)),
            }
            i = next;
            continue;
        }
        out.push(c);
        i += 1;
    }
    Ok(out)
}

/// A field value as a SQLite literal. Text is single-quoted with embedded
/// quotes doubled (SQLite gives backslashes no meaning inside literals); a
/// list becomes a parenthesized row value for `IN {list}`. An empty list is an
/// error rather than a query that silently matches nothing.
fn sql_value(value: &HostValue) -> Result<String> {
    Ok(match value {
        HostValue::Text(text) => sql_text(text)?,
        HostValue::Integer(n) => n.to_string(),
        HostValue::Bool(b) => if *b { "1" } else { "0" }.to_string(),
        HostValue::List(items) => {
            ensure!(
                !items.is_empty(),
                "an empty list has no SQL value; give at least one item"
            );
            let items = items
                .iter()
                .map(|item| sql_text(item))
                .collect::<Result<Vec<_>>>()?;
            format!("({})", items.join(", "))
        }
    })
}

/// Quote text as a SQLite string literal. NUL cannot appear in SQL text.
pub fn sql_text(text: &str) -> Result<String> {
    ensure!(
        !text.contains('\0'),
        "field value contains a NUL character, which a SQL literal cannot carry"
    );
    Ok(format!("'{}'", text.replace('\'', "''")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(query: &str, values: &[(&str, &str)]) -> std::result::Result<String, String> {
        fill_query(query, &mut |name| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| sql_text(value))
        })
        .map_err(|error| match error {
            FillError::Unknown(name) => format!("unknown {name}"),
            FillError::Other(error) => format!("{error:#}"),
        })
    }

    #[test]
    fn text_literals_escape_quotes_and_keep_backslashes() {
        assert_eq!(sql_text("it's").unwrap(), "'it''s'");
        assert_eq!(sql_text(r"a\'b\\").unwrap(), r"'a\''b\\'");
        assert_eq!(
            sql_text("'; DROP TABLE t; --").unwrap(),
            "'''; DROP TABLE t; --'"
        );
        assert!(sql_text("a\0b").unwrap_err().to_string().contains("NUL"));
    }

    #[test]
    fn query_substitutes_bare_placeholders_only() {
        assert_eq!(
            fill(
                "SELECT * FROM m WHERE a = {account} AND w = {workspace}",
                &[("account", "o'neil"), ("workspace", "w")]
            )
            .unwrap(),
            "SELECT * FROM m WHERE a = 'o''neil' AND w = 'w'"
        );
        // Comments are copied verbatim, so a value with a newline cannot
        // escape a line comment.
        assert_eq!(
            fill(
                "SELECT 1 -- {account}\n/* {account} */",
                &[("account", "x")]
            )
            .unwrap(),
            "SELECT 1 -- {account}\n/* {account} */"
        );
        // Text that is not a field name in braces is left alone.
        assert_eq!(
            fill("SELECT '{\"a\": 1}'", &[]).unwrap(),
            "SELECT '{\"a\": 1}'"
        );
        assert_eq!(
            fill("SELECT '{other}'", &[("account", "x")]).unwrap(),
            "SELECT '{other}'"
        );
    }

    #[test]
    fn query_rejects_quoted_and_unknown_placeholders() {
        for quoted in [
            "SELECT * FROM m WHERE a = '{account}'",
            "SELECT * FROM m WHERE a = 'it''s {account}'",
            "SELECT \"{account}\" FROM m",
            "SELECT [{account}] FROM m",
        ] {
            let error = fill(quoted, &[("account", "x")]).unwrap_err();
            assert!(
                error.contains("inside a quoted SQL string"),
                "{quoted}: {error}"
            );
        }
        assert_eq!(
            fill("SELECT {acount}", &[("account", "x")]).unwrap_err(),
            "unknown acount"
        );
    }
}
