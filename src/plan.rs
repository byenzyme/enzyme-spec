//! Reviewed changes to a program file: plan, then apply.
//!
//! A [`Plan`] replaces one `.enzyme` file in a config directory with a desired
//! text, written in full. It records the file's current revision (SHA-256 of
//! its bytes, or `"absent"`), the desired text and its digest, a unified diff,
//! a statement-level summary of the changes, and a `plan_id` over all of them.
//! [`plan`] is pure; [`ConfigStore`] is the filesystem side any host can use:
//! it chooses the file that holds a workspace, takes a lock, refuses stale or
//! altered plans, writes atomically, journals the write so an interrupted
//! apply is completed by the next caller, and keeps one receipt per applied
//! plan so applying the same plan again replays its receipt.
//!
//! State lives in `<configs>/.enzyme-apply/` by default: `lock`,
//! `journal.json` while a write is in flight, and `receipts/<plan_id>.json`.
//! The directory is ignored by [`crate::load_directory`], which reads only
//! `*.enzyme` files.
use crate::{Program, Reading, Source, Workspace, parse, resolve};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// JSON schema tag of a [`Plan`].
pub const PLAN_SCHEMA: &str = "enzyme.plan.v1";
/// JSON schema tag of a [`Receipt`].
pub const RECEIPT_SCHEMA: &str = "enzyme.receipt.v1";
/// The revision of a file that does not exist.
pub const ABSENT: &str = "absent";
/// Default state directory name inside the config directory.
pub const STATE_DIR: &str = ".enzyme-apply";

/// Hex SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The revision of a file's contents: SHA-256 of its bytes, or [`ABSENT`].
pub fn revision(contents: Option<&str>) -> String {
    contents.map_or_else(|| ABSENT.to_string(), |text| sha256(text.as_bytes()))
}

/// What kind of statement a [`Change`] touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Statement {
    Workspace,
    Source,
    Reading,
    Exclusion,
    Setting,
    Profile,
    /// A statement the summary does not model; the diff is exact.
    Program,
    /// Comments, ordering, or whitespace only.
    Layout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Added,
    Removed,
    Changed,
}

/// One statement-level change, for review. The plan's `diff` is exact; the
/// changes explain it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub statement: Statement,
    pub action: Action,
    /// The workspace the statement belongs to; `None` for file-level
    /// statements (profiles, settings, global learning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The statement's name: a source name, reading entity, `folder archive`,
    /// a setting or profile name.
    pub name: String,
    pub summary: String,
}

/// A reviewed replacement of one program file. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub schema: String,
    pub workspace: String,
    /// File name inside the config directory, e.g. `practice.enzyme`.
    pub target: String,
    pub base_revision: String,
    pub desired: String,
    pub desired_sha256: String,
    pub changes: Vec<Change>,
    pub diff: String,
    pub plan_id: String,
}

impl Plan {
    /// True when applying would not change the file.
    pub fn is_noop(&self) -> bool {
        self.base_revision == self.desired_sha256
    }
}

/// The record of one applied plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub schema: String,
    pub workspace: String,
    pub target: String,
    pub plan_id: String,
    /// SHA-256 of the plan JSON exactly as applied.
    pub request_hash: String,
    pub before_revision: String,
    pub after_revision: String,
    pub changes: Vec<Change>,
    /// True when this plan had already been applied and nothing was written.
    pub replayed: bool,
}

/// The in-flight write recorded before the target file is replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    pub receipt: Receipt,
    pub desired: String,
}

/// Why an apply was refused. Returned inside [`anyhow::Error`]; downcast to
/// branch on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyError {
    /// The target changed since the plan was made; plan again.
    Stale { expected: String, actual: String },
    /// The plan's contents do not match what planning produces.
    Altered(String),
    /// Unknown plan schema.
    Unsupported(String),
    /// An interrupted apply found the target in neither its before nor its
    /// after revision; resolve by hand, then remove the journal.
    Unrecoverable(String),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stale { expected, actual } => write!(
                f,
                "plan is stale: the target was {expected} when planned and is now {actual}; plan again"
            ),
            Self::Altered(why) => write!(f, "plan was altered: {why}"),
            Self::Unsupported(schema) => {
                write!(
                    f,
                    "unsupported plan schema {schema:?}; expected {PLAN_SCHEMA}"
                )
            }
            Self::Unrecoverable(why) => write!(f, "interrupted apply cannot be recovered: {why}"),
        }
    }
}

impl std::error::Error for ApplyError {}

/// Inputs to [`plan`].
pub struct PlanRequest<'a> {
    /// The workspace the desired text must declare.
    pub workspace: &'a str,
    /// File name inside the config directory.
    pub target: &'a str,
    /// The target's current text, or `None` when it does not exist.
    pub current: Option<&'a str>,
    pub desired: &'a str,
    /// Every other program in the config directory; the desired program must
    /// resolve together with them.
    pub namespace: &'a [Program],
}

/// Checks that a set of programs forms a valid namespace.
pub type Validator = dyn Fn(Vec<Program>) -> Result<()> + Send + Sync;

/// The default [`Validator`]: [`resolve`] against `user_home`.
pub fn resolver(user_home: PathBuf) -> Box<Validator> {
    Box::new(move |programs| resolve(programs, &user_home).map(drop))
}

/// Plan replacing `request.target` with `request.desired`. Pure apart from
/// what `validate` does.
pub fn plan(request: &PlanRequest<'_>, validate: &Validator) -> Result<Plan> {
    check_target(request.target)?;
    let workspace = request.workspace;
    let desired = parse(request.desired).context("desired program does not parse")?;
    match declared(&desired, workspace) {
        1 => {}
        0 => bail!("desired program does not declare workspace {workspace:?}"),
        _ => bail!("desired program declares workspace {workspace:?} more than once"),
    }
    for name in desired.workspaces.iter().map(|w| &w.name) {
        if request.namespace.iter().any(|p| declared(p, name) > 0) {
            bail!(
                "workspace {name:?} is declared in another config file than {}",
                request.target
            );
        }
    }
    let current = match request.current {
        None => None,
        Some(text) => match parse(text) {
            Ok(program) => {
                if declared(&program, workspace) == 0 {
                    bail!(
                        "{} exists and does not declare workspace {workspace:?}",
                        request.target
                    );
                }
                Some(Ok(program))
            }
            Err(error) => Some(Err(error)),
        },
    };
    let mut programs = request.namespace.to_vec();
    programs.push(desired.clone());
    validate(programs).context("desired program does not resolve with the other config files")?;

    let changes = match &current {
        None => changes(&Program::default(), &desired, None, Some(request.desired)),
        Some(Ok(current)) => changes(current, &desired, request.current, Some(request.desired)),
        Some(Err(error)) => vec![Change {
            statement: Statement::Program,
            action: Action::Changed,
            workspace: None,
            name: request.target.to_string(),
            summary: format!("Replace a file that does not parse ({error})"),
        }],
    };
    let base_revision = revision(request.current);
    let desired_sha256 = sha256(request.desired.as_bytes());
    let plan_id = plan_id(
        workspace,
        request.target,
        &base_revision,
        &desired_sha256,
        &changes,
    )?;
    Ok(Plan {
        schema: PLAN_SCHEMA.to_string(),
        workspace: workspace.to_string(),
        target: request.target.to_string(),
        base_revision,
        desired: request.desired.to_string(),
        desired_sha256,
        changes,
        diff: unified_diff(
            request.current.unwrap_or(""),
            request.desired,
            request.target,
        ),
        plan_id,
    })
}

/// A unified diff between two program texts, labelled with `path`.
pub fn unified_diff(before: &str, after: &str, path: &str) -> String {
    similar::TextDiff::from_lines(before, after)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
}

fn plan_id(
    workspace: &str,
    target: &str,
    base_revision: &str,
    desired_sha256: &str,
    changes: &[Change],
) -> Result<String> {
    #[derive(Serialize)]
    struct Identity<'a> {
        schema: &'a str,
        workspace: &'a str,
        target: &'a str,
        base_revision: &'a str,
        desired_sha256: &'a str,
        changes: &'a [Change],
    }
    let bytes = serde_json::to_vec(&Identity {
        schema: PLAN_SCHEMA,
        workspace,
        target,
        base_revision,
        desired_sha256,
        changes,
    })?;
    Ok(sha256(&bytes))
}

fn check_target(target: &str) -> Result<()> {
    if target.is_empty()
        || target.starts_with('.')
        || !target.ends_with(".enzyme")
        || target.contains(['/', '\\'])
        || target.chars().any(char::is_control)
    {
        bail!("plan target {target:?} must be a plain .enzyme file name");
    }
    Ok(())
}

fn declared(program: &Program, workspace: &str) -> usize {
    program
        .workspaces
        .iter()
        .filter(|w| w.name == workspace)
        .count()
}

/// The default file name for a new workspace: its name with anything other
/// than ASCII letters, digits, `-`, `_` and `.` replaced by `-`.
pub fn default_target(workspace: &str) -> String {
    let mut stem: String = workspace
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    stem = stem.trim_start_matches('.').to_string();
    if stem.is_empty() {
        stem = "workspace".to_string();
    }
    format!("{stem}.enzyme")
}

// ---------------------------------------------------------------- summaries

fn change(
    statement: Statement,
    action: Action,
    workspace: Option<&str>,
    name: impl Into<String>,
    summary: impl Into<String>,
) -> Change {
    Change {
        statement,
        action,
        workspace: workspace.map(str::to_string),
        name: name.into(),
        summary: summary.into(),
    }
}

fn changes(
    before: &Program,
    after: &Program,
    before_text: Option<&str>,
    after_text: Option<&str>,
) -> Vec<Change> {
    let mut out = Vec::new();
    keyed(
        &before.profiles,
        &after.profiles,
        |name, action| {
            let verb = verb(action);
            change(
                Statement::Profile,
                action,
                None,
                name,
                format!("{verb} profile {name}"),
            )
        },
        &mut out,
    );
    settings(before, after, &mut out);
    let before_vaults: BTreeMap<_, _> = before.vaults.iter().map(|v| (v.path.clone(), v)).collect();
    let after_vaults: BTreeMap<_, _> = after.vaults.iter().map(|v| (v.path.clone(), v)).collect();
    keyed(
        &before_vaults,
        &after_vaults,
        |path, action| {
            let verb = verb(action);
            change(
                Statement::Workspace,
                action,
                None,
                path,
                format!("{verb} vault {path}"),
            )
        },
        &mut out,
    );
    let before_ws: BTreeMap<_, _> = before
        .workspaces
        .iter()
        .map(|w| (w.name.clone(), w))
        .collect();
    let after_ws: BTreeMap<_, _> = after
        .workspaces
        .iter()
        .map(|w| (w.name.clone(), w))
        .collect();
    for (name, workspace) in &before_ws {
        if !after_ws.contains_key(name) {
            out.push(change(
                Statement::Workspace,
                Action::Removed,
                Some(name),
                name.as_str(),
                format!("Remove workspace {name:?}"),
            ));
        } else if after_ws[name] != *workspace {
            workspace_changes(workspace, after_ws[name], &mut out);
        }
    }
    for (name, workspace) in &after_ws {
        if !before_ws.contains_key(name) {
            out.push(change(
                Statement::Workspace,
                Action::Added,
                Some(name),
                name.as_str(),
                format!("Create workspace {name:?}"),
            ));
            workspace_changes(&Workspace::default(), workspace, &mut out);
        }
    }
    if out.is_empty() && before != after {
        // A statement this summary does not model (for example one added to
        // the grammar after it was written).
        out.push(change(
            Statement::Program,
            Action::Changed,
            None,
            "program",
            "Change program statements; see diff",
        ));
    }
    if out.is_empty() && before_text != after_text {
        out.push(change(
            Statement::Layout,
            Action::Changed,
            None,
            "layout",
            "Change comments or layout only",
        ));
    }
    out
}

fn verb(action: Action) -> &'static str {
    match action {
        Action::Added => "Add",
        Action::Removed => "Remove",
        Action::Changed => "Change",
    }
}

fn keyed<V: PartialEq>(
    before: &BTreeMap<String, V>,
    after: &BTreeMap<String, V>,
    make: impl Fn(&str, Action) -> Change,
    out: &mut Vec<Change>,
) {
    for (key, value) in before {
        match after.get(key) {
            None => out.push(make(key, Action::Removed)),
            Some(other) if other != value => out.push(make(key, Action::Changed)),
            Some(_) => {}
        }
    }
    for key in after.keys() {
        if !before.contains_key(key) {
            out.push(make(key, Action::Added));
        }
    }
}

fn setting<T: PartialEq + std::fmt::Debug>(
    workspace: Option<&str>,
    name: &str,
    before: &T,
    after: &T,
    out: &mut Vec<Change>,
) {
    if before != after {
        let scope = workspace.map_or(String::new(), |w| format!(" in workspace {w:?}"));
        out.push(change(
            Statement::Setting,
            Action::Changed,
            workspace,
            name,
            format!("Change {name}{scope}"),
        ));
    }
}

fn settings(before: &Program, after: &Program, out: &mut Vec<Change>) {
    let (b, a) = (&before.settings, &after.settings);
    let start = out.len();
    setting(None, "generation", &b.generation, &a.generation, out);
    setting(None, "local model", &b.local_model, &a.local_model, out);
    setting(None, "updates", &b.updates, &a.updates, out);
    setting(
        None,
        "embedding limit",
        &b.embedding_limit,
        &a.embedding_limit,
        out,
    );
    setting(None, "minimum tags", &b.minimum_tags, &a.minimum_tags, out);
    setting(
        None,
        "minimum links",
        &b.minimum_links,
        &a.minimum_links,
        out,
    );
    setting(
        None,
        "minimum folders",
        &b.minimum_folders,
        &a.minimum_folders,
        out,
    );
    setting(None, "total limit", &b.total_limit, &a.total_limit, out);
    setting(None, "format", &b.format, &a.format, out);
    if out.len() == start {
        // Fields added to Settings after this summary was written.
        setting(None, "settings", b, a, out);
    }
    setting(None, "learning", &before.learning, &after.learning, out);
    setting(None, "when asked", &before.retrieval, &after.retrieval, out);
}

fn reading_key(reading: &Reading) -> String {
    if reading.pattern {
        format!("{} matching", reading.entity)
    } else {
        reading.entity.clone()
    }
}

fn reading_summary(reading: &Reading) -> String {
    let mut text = reading_key(reading);
    if reading.include_linked_pages {
        text.push_str(" including linked pages");
    }
    if reading.include_who_links {
        text.push_str(" including who links");
    }
    if reading.profile != "auto" {
        text.push_str(&format!(" about {}", reading.profile));
    }
    text
}

fn workspace_changes(before: &Workspace, after: &Workspace, out: &mut Vec<Change>) {
    let ws = Some(after.name.as_str());
    let start = out.len();
    let before_sources: BTreeMap<_, _> = before
        .sources
        .iter()
        .map(|s| (s.name().to_string(), s))
        .collect();
    let after_sources: BTreeMap<_, _> = after
        .sources
        .iter()
        .map(|s| (s.name().to_string(), s))
        .collect();
    let kind_of = |name: &str| -> String {
        after_sources
            .get(name)
            .or_else(|| before_sources.get(name))
            .map_or_else(String::new, |s: &&Source| s.kind().to_string())
    };
    keyed(
        &before_sources,
        &after_sources,
        |name, action| {
            let kind = kind_of(name);
            let detail = match (action, before_sources.get(name), after_sources.get(name)) {
                (Action::Changed, Some(b), Some(a)) if b.kind() != a.kind() => {
                    format!(" from {} to {}", b.kind(), a.kind())
                }
                _ => String::new(),
            };
            change(
                Statement::Source,
                action,
                ws,
                name,
                format!("{} {kind} source {name:?}{detail}", verb(action)),
            )
        },
        out,
    );

    let before_readings: BTreeMap<_, _> = before
        .readings
        .iter()
        .map(|r| (reading_key(r), r))
        .collect();
    let after_readings: BTreeMap<_, _> =
        after.readings.iter().map(|r| (reading_key(r), r)).collect();
    keyed(
        &before_readings,
        &after_readings,
        |key, action| {
            let reading = after_readings
                .get(key)
                .or_else(|| before_readings.get(key))
                .unwrap();
            let summary = match action {
                Action::Added => format!("Learn questions from {}", reading_summary(reading)),
                Action::Removed => format!("Stop learning questions from {key}"),
                Action::Changed => format!("Change reading of {key}: {}", reading_summary(reading)),
            };
            change(Statement::Reading, action, ws, key, summary)
        },
        out,
    );
    if out.len() == start
        && before.readings != after.readings
        && before.readings.len() == after.readings.len()
    {
        out.push(change(
            Statement::Reading,
            Action::Changed,
            ws,
            "readings",
            "Reorder readings",
        ));
    }

    for (kind, b, a) in [
        ("folder", &before.exclusions, &after.exclusions),
        ("tag", &before.excluded_tags, &after.excluded_tags),
        ("link", &before.excluded_links, &after.excluded_links),
    ] {
        let b: BTreeSet<_> = b.iter().collect();
        let a: BTreeSet<_> = a.iter().collect();
        for value in b.difference(&a) {
            out.push(change(
                Statement::Exclusion,
                Action::Removed,
                ws,
                format!("{kind} {value}"),
                format!("Stop leaving out {kind} {value:?}"),
            ));
        }
        for value in a.difference(&b) {
            out.push(change(
                Statement::Exclusion,
                Action::Added,
                ws,
                format!("{kind} {value}"),
                format!("Leave out {kind} {value:?}"),
            ));
        }
    }

    setting(ws, "learning", &before.learning, &after.learning, out);
    setting(ws, "format", &before.format, &after.format, out);
    setting(ws, "targets", &before.targets, &after.targets, out);
    setting(
        ws,
        "link fields",
        &before.frontmatter_link_fields,
        &after.frontmatter_link_fields,
        out,
    );
    setting(
        ws,
        "embedding limit",
        &before.embedding_limit,
        &after.embedding_limit,
        out,
    );
    setting(ws, "memories", &before.memories, &after.memories, out);
    setting(ws, "when asked", &before.retrieval, &after.retrieval, out);
    setting(
        ws,
        "create note",
        &before.note_policies,
        &after.note_policies,
        out,
    );

    if out.len() == start && !before.name.is_empty() {
        out.push(change(
            Statement::Workspace,
            Action::Changed,
            ws,
            after.name.as_str(),
            format!("Change workspace {:?}; see diff", after.name),
        ));
    }
}

// --------------------------------------------------------------- filesystem

/// A config directory plus the state that makes applies safe. See the module
/// docs for the layout.
pub struct ConfigStore {
    configs: PathBuf,
    state: PathBuf,
    validate: Box<Validator>,
}

impl ConfigStore {
    /// A store over `configs`, validating with [`resolver`]`(user_home)` and
    /// keeping state in `configs/.enzyme-apply`.
    pub fn new(configs: impl Into<PathBuf>, user_home: impl Into<PathBuf>) -> Self {
        let configs = configs.into();
        Self {
            state: configs.join(STATE_DIR),
            configs,
            validate: resolver(user_home.into()),
        }
    }

    /// Keep lock, journal, and receipts in `state` instead.
    pub fn with_state_dir(mut self, state: impl Into<PathBuf>) -> Self {
        self.state = state.into();
        self
    }

    /// Validate namespaces with `validate` instead of plain [`resolve`], for
    /// hosts that lower their own source kinds first.
    pub fn with_validator(mut self, validate: Box<Validator>) -> Self {
        self.validate = validate;
        self
    }

    pub fn configs(&self) -> &Path {
        &self.configs
    }

    pub fn state(&self) -> &Path {
        &self.state
    }

    /// The file holding `workspace`: the one config file that declares it,
    /// otherwise [`default_target`] for a new workspace. Also returns every
    /// other program in the directory.
    pub fn locate(&self, workspace: &str) -> Result<(String, Vec<Program>)> {
        let mut holding = Vec::new();
        let mut others = Vec::new();
        let mut broken = Vec::new();
        for path in enzyme_files(&self.configs)? {
            let name = file_name(&path)?;
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            match parse(&text) {
                Ok(program) if declared(&program, workspace) > 0 => holding.push(name),
                Ok(program) => others.push((name, program)),
                Err(error) => broken.push((name, error)),
            }
        }
        let fallback = default_target(workspace);
        let target = match holding.as_slice() {
            [one] => one.clone(),
            [] if broken.iter().any(|(name, _)| *name == fallback) => fallback,
            [] => {
                if others.iter().any(|(name, _)| *name == fallback) {
                    bail!(
                        "{fallback} exists in {} but does not declare workspace {workspace:?}",
                        self.configs.display()
                    );
                }
                fallback
            }
            many => bail!(
                "workspace {workspace:?} is declared in several config files: {}",
                many.join(", ")
            ),
        };
        if let Some((name, error)) = broken.into_iter().find(|(name, _)| *name != target) {
            return Err(error.context(format!("invalid config file {name}")));
        }
        Ok((target, others.into_iter().map(|(_, p)| p).collect()))
    }

    /// Plan replacing `workspace`'s file with `desired`. Completes any
    /// interrupted apply first so the base revision is current.
    pub fn plan(&self, workspace: &str, desired: &str) -> Result<Plan> {
        let _lock = self.lock()?;
        self.recover_locked()?;
        self.plan_locked(workspace, desired)
    }

    fn plan_locked(&self, workspace: &str, desired: &str) -> Result<Plan> {
        let (target, namespace) = self.locate(workspace)?;
        let current = read_optional(&self.configs.join(&target))?;
        plan(
            &PlanRequest {
                workspace,
                target: &target,
                current: current.as_deref(),
                desired,
                namespace: &namespace,
            },
            &*self.validate,
        )
    }

    /// Apply `plan`: write exactly `plan.desired` to its target, only if the
    /// target is still at `plan.base_revision` and the plan is exactly what
    /// planning produces now. Applying an already-applied plan returns its
    /// receipt with `replayed: true` and writes nothing.
    pub fn apply(&self, plan: &Plan) -> Result<Receipt> {
        let journal = self.validate_and_prepare(plan)?;
        let Some((journal, _lock)) = journal else {
            return self.replay(plan);
        };
        self.commit(&journal)?;
        Ok(journal.receipt)
    }

    /// Complete an apply that was interrupted after its journal was written.
    /// Returns the completed receipt, if there was one.
    pub fn recover(&self) -> Result<Option<Receipt>> {
        let _lock = self.lock()?;
        self.recover_locked()
    }

    /// Validate `plan` and journal it without writing the target, as an apply
    /// interrupted at that point would leave things. For testing recovery.
    #[doc(hidden)]
    pub fn prepare(&self, plan: &Plan) -> Result<Journal> {
        match self.validate_and_prepare(plan)? {
            Some((journal, _lock)) => Ok(journal),
            None => bail!("plan {} was already applied", plan.plan_id),
        }
    }

    /// `None` when the plan was already applied (a matching receipt exists).
    fn validate_and_prepare(&self, plan: &Plan) -> Result<Option<(Journal, std::fs::File)>> {
        if plan.schema != PLAN_SCHEMA {
            return Err(ApplyError::Unsupported(plan.schema.clone()).into());
        }
        check_target(&plan.target).map_err(|e| ApplyError::Altered(e.to_string()))?;
        if plan.plan_id.len() != 64 || !plan.plan_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(ApplyError::Altered("plan_id is not a SHA-256".into()).into());
        }
        if sha256(plan.desired.as_bytes()) != plan.desired_sha256 {
            return Err(ApplyError::Altered("desired does not match desired_sha256".into()).into());
        }
        let request_hash = sha256(&serde_json::to_vec(plan)?);
        let lock = self.lock()?;
        self.recover_locked()?;
        if let Some(receipt) = self.load_receipt(&plan.plan_id)? {
            if receipt.request_hash != request_hash {
                return Err(ApplyError::Altered(
                    "a plan with this plan_id but different contents was applied".into(),
                )
                .into());
            }
            return Ok(None);
        }
        let (target, _) = self.locate(&plan.workspace)?;
        if target != plan.target {
            return Err(ApplyError::Altered(format!(
                "workspace {:?} lives in {target}, not {}",
                plan.workspace, plan.target
            ))
            .into());
        }
        let current = read_optional(&self.configs.join(&target))?;
        let actual = revision(current.as_deref());
        if actual != plan.base_revision {
            return Err(ApplyError::Stale {
                expected: plan.base_revision.clone(),
                actual,
            }
            .into());
        }
        let rebuilt = self.plan_locked(&plan.workspace, &plan.desired)?;
        if rebuilt != *plan {
            return Err(ApplyError::Altered(
                "changes, diff, or plan_id differ from what planning produces".into(),
            )
            .into());
        }
        let journal = Journal {
            receipt: Receipt {
                schema: RECEIPT_SCHEMA.to_string(),
                workspace: plan.workspace.clone(),
                target: plan.target.clone(),
                plan_id: plan.plan_id.clone(),
                request_hash,
                before_revision: plan.base_revision.clone(),
                after_revision: plan.desired_sha256.clone(),
                changes: plan.changes.clone(),
                replayed: false,
            },
            desired: plan.desired.clone(),
        };
        write_atomic(&self.journal_path(), &serde_json::to_vec_pretty(&journal)?)?;
        Ok(Some((journal, lock)))
    }

    fn replay(&self, plan: &Plan) -> Result<Receipt> {
        let mut receipt = self
            .load_receipt(&plan.plan_id)?
            .context("receipt disappeared during replay")?;
        receipt.replayed = true;
        Ok(receipt)
    }

    /// Write the target, store the receipt, clear the journal. Caller holds
    /// the lock and the target is at the journal's before revision.
    fn commit(&self, journal: &Journal) -> Result<()> {
        write_atomic(
            &self.target_path(&journal.receipt.target)?,
            journal.desired.as_bytes(),
        )?;
        self.finish(journal)
    }

    fn finish(&self, journal: &Journal) -> Result<()> {
        let path = self.receipt_path(&journal.receipt.plan_id);
        if !path.exists() {
            write_atomic(&path, &serde_json::to_vec_pretty(&journal.receipt)?)?;
        }
        let journal_path = self.journal_path();
        std::fs::remove_file(&journal_path)
            .with_context(|| format!("removing {}", journal_path.display()))?;
        sync_dir(&self.state);
        Ok(())
    }

    fn recover_locked(&self) -> Result<Option<Receipt>> {
        let path = self.journal_path();
        let Some(bytes) = read_optional_bytes(&path)? else {
            return Ok(None);
        };
        let journal: Journal = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid apply journal {}", path.display()))?;
        let receipt = &journal.receipt;
        check_target(&receipt.target)?;
        if sha256(journal.desired.as_bytes()) != receipt.after_revision {
            return Err(ApplyError::Unrecoverable(format!(
                "journal {} does not match its after revision",
                path.display()
            ))
            .into());
        }
        let current = read_optional(&self.configs.join(&receipt.target))?;
        let actual = revision(current.as_deref());
        if actual == receipt.before_revision {
            self.commit(&journal)?;
        } else if actual == receipt.after_revision {
            self.finish(&journal)?;
        } else {
            return Err(ApplyError::Unrecoverable(format!(
                "{} is at {actual}, neither {} nor {}; resolve it and remove {}",
                receipt.target,
                receipt.before_revision,
                receipt.after_revision,
                path.display()
            ))
            .into());
        }
        Ok(Some(journal.receipt))
    }

    fn lock(&self) -> Result<std::fs::File> {
        use fs2::FileExt;
        std::fs::create_dir_all(&self.state)
            .with_context(|| format!("creating {}", self.state.display()))?;
        let path = self.state.join("lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        file.lock_exclusive()
            .with_context(|| format!("locking {}", path.display()))?;
        Ok(file)
    }

    fn journal_path(&self) -> PathBuf {
        self.state.join("journal.json")
    }

    fn receipt_path(&self, plan_id: &str) -> PathBuf {
        self.state.join("receipts").join(format!("{plan_id}.json"))
    }

    fn load_receipt(&self, plan_id: &str) -> Result<Option<Receipt>> {
        let path = self.receipt_path(plan_id);
        read_optional_bytes(&path)?
            .map(|bytes| {
                serde_json::from_slice(&bytes)
                    .with_context(|| format!("invalid receipt {}", path.display()))
            })
            .transpose()
    }

    /// The file to replace: a symlinked program is written through its link.
    fn target_path(&self, target: &str) -> Result<PathBuf> {
        let path = self.configs.join(target);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(&path)
                .with_context(|| format!("following {}", path.display())),
            _ => Ok(path),
        }
    }
}

fn enzyme_files(directory: &Path) -> Result<Vec<PathBuf>> {
    if !directory.exists() {
        return Ok(vec![]);
    }
    let mut paths = std::fs::read_dir(directory)
        .with_context(|| format!("reading {}", directory.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.retain(|p| p.extension().is_some_and(|e| e == "enzyme") && p.is_file());
    paths.sort();
    Ok(paths)
}

fn file_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
        .with_context(|| format!("config file name is not UTF-8: {}", path.display()))
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    read_optional_bytes(path)?
        .map(|bytes| {
            String::from_utf8(bytes).with_context(|| format!("{} is not UTF-8", path.display()))
        })
        .transpose()
}

fn read_optional_bytes(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

/// Write via a synced temporary file in the same directory, rename, then sync
/// the directory. The temporary name never ends in `.enzyme`.
fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    let parent = path.parent().context("path needs a parent directory")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let temporary = parent.join(format!(
        ".enzyme-apply-{}-{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(contents)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.with_context(|| format!("writing {}", path.display()))?;
    sync_dir(parent);
    Ok(())
}

/// Best effort: persist a rename. Directories cannot be opened for syncing on
/// every platform.
fn sync_dir(directory: &Path) {
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(directory) {
        let _ = dir.sync_all();
    }
    #[cfg(not(unix))]
    let _ = directory;
}
