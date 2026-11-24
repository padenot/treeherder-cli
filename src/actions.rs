//! Taskcluster in-tree actions: the client side of Treeherder's
//! ui/models/taskcluster.js. Loads a decision task's actions.json, selects an
//! action for a task, and triggers its hook.

use crate::api::TC_ROOT_URL;
use crate::util::{scope_satisfies, write_atomic};
use anyhow::{anyhow, bail, Context, Result};
use reqwest::{Client, StatusCode};
use serde::de::IgnoredAny;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const CACHE_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Deserialize)]
pub struct ActionsJson {
    pub version: u32,
    #[serde(default)]
    pub variables: Map<String, Value>,
    pub actions: Vec<Action>,
}

#[derive(Debug, Deserialize)]
pub struct Action {
    pub name: String,
    pub kind: String,
    pub context: Vec<HashMap<String, String>>,
    #[serde(rename = "hookGroupId")]
    pub hook_group_id: Option<String>,
    #[serde(rename = "hookId")]
    pub hook_id: Option<String>,
    #[serde(rename = "hookPayload")]
    pub hook_payload: Option<Value>,
}

#[derive(Deserialize)]
struct IndexedTask {
    #[serde(rename = "taskId")]
    task_id: String,
}

#[derive(Deserialize)]
struct TaskDefinition {
    #[serde(default)]
    tags: HashMap<String, String>,
    #[serde(default)]
    scopes: Vec<String>,
}

fn cache_root_from_env(
    xdg_cache_home: Option<PathBuf>,
    platform_dir: Option<PathBuf>,
) -> Option<PathBuf> {
    xdg_cache_home
        .filter(|dir| dir.is_absolute())
        .or(platform_dir)
        .map(|dir| dir.join("treeherder-cli").join("taskcluster-actions"))
}

fn cache_root() -> Option<PathBuf> {
    cache_root_from_env(
        std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from),
        if cfg!(windows) {
            dirs::cache_dir()
        } else {
            dirs::home_dir().map(|home| home.join(".cache"))
        },
    )
}

fn read_cache(path: &Path) -> Option<String> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    if modified.elapsed().is_ok_and(|age| age > CACHE_MAX_AGE) {
        return None;
    }
    fs::read_to_string(path).ok()
}

fn write_cache(path: &Path, contents: &[u8]) {
    if write_atomic(path, contents, false).is_ok() {
        if let Some(dir) = path.parent() {
            prune_stale(dir, SystemTime::now());
        }
    }
}

fn prune_stale(dir: &Path, now: SystemTime) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json")
            && entry.file_type().is_ok_and(|ty| ty.is_file())
            && entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| {
                    now.duration_since(modified)
                        .is_ok_and(|age| age > CACHE_MAX_AGE)
                })
        {
            let _ = fs::remove_file(path);
        }
    }
}

async fn get_text(client: &Client, url: &str) -> Result<String> {
    Ok(client
        .get(url)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?)
}

/// Returns `None` when the decision task has not completed yet (the index route
/// is only inserted on completion) or does not exist. Resolve the mutable index each time.
pub async fn find_decision_task(
    client: &Client,
    repo: &str,
    revision: &str,
) -> Result<Option<String>> {
    let url = format!(
        "{TC_ROOT_URL}/api/index/v1/task/gecko.v2.{repo}.revision.{revision}.taskgraph.decision"
    );
    let response = client.get(&url).timeout(REQUEST_TIMEOUT).send().await?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let indexed: IndexedTask = response.error_for_status()?.json().await?;
    anyhow::ensure!(
        is_task_id(&indexed.task_id),
        "Taskcluster index returned an invalid task ID"
    );
    Ok(Some(indexed.task_id))
}

pub struct DecisionRun {
    pub task_id: String,
    run_id: u32,
}

impl DecisionRun {
    fn cache_path(&self, kind: &str) -> Option<PathBuf> {
        cache_root().map(|root| {
            root.join(kind)
                .join(format!("{}-{}.json", self.task_id, self.run_id))
        })
    }

    fn artifact_url(&self, name: &str) -> String {
        format!(
            "{TC_ROOT_URL}/api/queue/v1/task/{}/runs/{}/artifacts/public/{name}",
            self.task_id, self.run_id
        )
    }
}

pub async fn fetch_decision_run(client: &Client, task_id: String) -> Result<Option<DecisionRun>> {
    #[derive(Deserialize)]
    struct Response {
        status: DecisionStatus,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct DecisionStatus {
        task_id: String,
        state: String,
        runs: Vec<Run>,
    }

    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Run {
        run_id: u32,
    }

    anyhow::ensure!(
        is_task_id(&task_id),
        "Invalid Taskcluster task ID {task_id}"
    );
    let url = format!("{TC_ROOT_URL}/api/queue/v1/task/{task_id}/status");
    let response: Response = client
        .get(url)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let status = response.status;
    anyhow::ensure!(
        status.task_id == task_id,
        "Taskcluster returned a status for a different task"
    );
    match status.state.as_str() {
        "completed" => {
            let run_id = status
                .runs
                .last()
                .context("Completed decision task has no runs")?
                .run_id;
            Ok(Some(DecisionRun { task_id, run_id }))
        }
        "unscheduled" | "pending" | "running" => Ok(None),
        state => bail!("The latest run of decision task {task_id} ended in {state}"),
    }
}

pub async fn fetch_actions(client: &Client, decision: &DecisionRun) -> Result<ActionsJson> {
    let path = decision.cache_path("actions");
    if let Some(actions) = path
        .as_deref()
        .and_then(read_cache)
        .and_then(|text| parse_actions(&text).ok())
    {
        return Ok(actions);
    }

    let url = decision.artifact_url("actions.json");
    let text = get_text(client, &url).await?;
    let actions = parse_actions(&text)?;
    if let Some(path) = path {
        write_cache(&path, text.as_bytes());
    }
    Ok(actions)
}

fn parse_actions(text: &str) -> Result<ActionsJson> {
    let actions: ActionsJson = serde_json::from_str(text).context("parsing actions.json")?;
    if actions.version != 1 {
        bail!("unsupported actions.json version {}", actions.version);
    }
    Ok(actions)
}

pub async fn fetch_runnable_labels(client: &Client, decision: &DecisionRun) -> Result<Vec<String>> {
    let path = decision.cache_path("runnable");
    if let Some(labels) = path
        .as_deref()
        .and_then(read_cache)
        .and_then(|text| serde_json::from_str(&text).ok())
    {
        return Ok(labels);
    }

    let url = decision.artifact_url("runnable-jobs.json");
    let text = get_text(client, &url).await?;
    // Only the keys (task labels) are needed, so the values are skipped.
    let runnable: BTreeMap<String, IgnoredAny> =
        serde_json::from_str(&text).context("parsing runnable-jobs.json")?;
    let labels: Vec<String> = runnable.into_keys().collect();
    if let (Some(path), Ok(data)) = (path, serde_json::to_vec(&labels)) {
        write_cache(&path, &data);
    }
    Ok(labels)
}

fn is_task_id(id: &str) -> bool {
    id.len() == 22
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The tags of each task, keyed by task ID.
pub async fn fetch_task_tags<'a>(
    queue: &taskcluster::Queue,
    task_ids: impl IntoIterator<Item = &'a str>,
) -> Result<HashMap<String, HashMap<String, String>>> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Entry {
        task_id: String,
        task: TaskDefinition,
    }
    let task_ids: BTreeSet<&str> = task_ids.into_iter().collect();
    let payload = json!({ "taskIds": task_ids });
    let payload = &payload;
    let entries: Vec<Entry> =
        collect_pages(|token| async move { queue.tasks(payload, token.as_deref(), None).await })
            .await?;
    Ok(match_requested(&task_ids, entries, |entry| &entry.task_id)?
        .into_iter()
        .map(|entry| (entry.task_id, entry.task.tags))
        .collect())
}

async fn fetch_task_definition(client: &Client, task_id: &str) -> Result<TaskDefinition> {
    anyhow::ensure!(is_task_id(task_id), "Invalid Taskcluster task ID {task_id}");
    let url = format!("{TC_ROOT_URL}/api/queue/v1/task/{task_id}");
    Ok(client
        .get(&url)
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

pub async fn verify_action_hooks<'a>(
    client: &Client,
    decision_task_id: &str,
    actions: impl Iterator<Item = &'a Action>,
) -> Result<()> {
    let required: BTreeSet<_> = actions.map(Action::decision_scope).collect::<Result<_>>()?;
    let definition = fetch_task_definition(client, decision_task_id).await?;
    let auth = taskcluster::Auth::new(taskcluster::ClientBuilder::new(TC_ROOT_URL))?;
    let expanded = auth
        .expandScopes(&json!({ "scopes": definition.scopes }))
        .await?;
    #[derive(Deserialize)]
    struct Scopes {
        scopes: Vec<String>,
    }
    let expanded: Scopes = serde_json::from_value(expanded)?;
    for scope in required {
        anyhow::ensure!(
            expanded
                .scopes
                .iter()
                .any(|granted| scope_satisfies(granted, &scope)),
            "Decision task {decision_task_id} does not authorize {scope}"
        );
    }
    Ok(())
}

/// One page of a Taskcluster list endpoint: listTaskGroup, tasks or statuses.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page<T> {
    #[serde(alias = "statuses")]
    tasks: Vec<T>,
    continuation_token: Option<String>,
}

async fn collect_pages<T, Fut>(mut fetch: impl FnMut(Option<String>) -> Fut) -> Result<Vec<T>>
where
    T: DeserializeOwned,
    Fut: Future<Output = Result<Value>>,
{
    let mut token = None;
    let mut items = Vec::new();
    loop {
        let page: Page<T> = serde_json::from_value(fetch(token.take()).await?)?;
        items.extend(page.tasks);
        match page.continuation_token {
            Some(next) => token = Some(next),
            None => return Ok(items),
        }
    }
}

/// The queue's batch endpoints skip unknown task IDs; insist on exactly the requested ones.
fn match_requested<T>(
    requested: &BTreeSet<&str>,
    items: Vec<T>,
    task_id: fn(&T) -> &str,
) -> Result<Vec<T>> {
    let items: Vec<T> = items
        .into_iter()
        .filter(|item| requested.contains(task_id(item)))
        .collect();
    let found: HashSet<&str> = items.iter().map(task_id).collect();
    if let Some(missing) = requested.iter().find(|id| !found.contains(**id)) {
        bail!("Taskcluster has no task {missing}");
    }
    Ok(items)
}

impl ActionsJson {
    fn hook_actions(&self, name: &str) -> Vec<&Action> {
        self.actions
            .iter()
            .filter(|action| action.kind == "hook" && action.name == name)
            .collect()
    }

    fn no_such_action(&self, name: &str) -> anyhow::Error {
        let names: BTreeSet<&str> = self
            .actions
            .iter()
            .filter(|action| action.kind == "hook")
            .map(|action| action.name.as_str())
            .collect();
        anyhow!(
            "no action named '{name}'; available: {}",
            names.into_iter().collect::<Vec<_>>().join(", ")
        )
    }

    /// The highest SCM level among the hook actions with these names, known
    /// before choosing their target tasks.
    pub fn level(&self, names: &[&str]) -> Result<u8> {
        let mut level = None;
        for action in names.iter().flat_map(|name| self.hook_actions(name)) {
            level = level.max(Some(action.level()?));
        }
        level.ok_or_else(|| self.no_such_action(names[0]))
    }

    /// Like Treeherder's getAction: the first hook action with this name that
    /// applies to the target task (or to the push, without a task) wins.
    pub fn find(&self, name: &str, task_tags: Option<&HashMap<String, String>>) -> Result<&Action> {
        let candidates = self.hook_actions(name);
        if candidates.is_empty() {
            return Err(self.no_such_action(name));
        }

        candidates
            .into_iter()
            .find(|action| action.applies_to(task_tags))
            .ok_or_else(|| match task_tags {
                Some(_) => anyhow!("action '{name}' does not apply to this task"),
                None => anyhow!("action '{name}' needs a target task"),
            })
    }
}

impl Action {
    fn decision_scope(&self) -> Result<String> {
        let (group, hook) = self.hook()?;
        anyhow::ensure!(
            !group.contains('*') && !hook.contains('*'),
            "Action refers to a wildcard hook"
        );
        Ok(format!("in-tree:hook-action:{group}/{hook}"))
    }

    fn hook(&self) -> Result<(&str, &str)> {
        let group = self
            .hook_group_id
            .as_deref()
            .context("action has no hookGroupId")?;
        let hook = self.hook_id.as_deref().context("action has no hookId")?;
        Ok((group, hook))
    }

    fn applies_to(&self, task_tags: Option<&HashMap<String, String>>) -> bool {
        match task_tags {
            None => self.context.is_empty(),
            Some(tags) => self.context.iter().any(|tag_set| {
                tag_set
                    .iter()
                    .all(|(key, value)| tags.get(key) == Some(value))
            }),
        }
    }

    /// The supported SCM level from a hookId like `in-tree-action-1-generic/b936906e1a`.
    pub fn level(&self) -> Result<u8> {
        let hook_id = self.hook_id.as_deref().context("action has no hookId")?;
        let level = hook_id
            .strip_prefix("in-tree-action-")
            .and_then(|rest| rest.split('-').next())
            .and_then(|level| level.parse::<u8>().ok())
            .with_context(|| format!("cannot parse SCM level from hookId {hook_id}"))?;
        anyhow::ensure!(
            matches!(level, 1 | 3),
            "Taskcluster actions on SCM level {level} are unsupported. Supported SCM levels are 1 and 3."
        );
        Ok(level)
    }

    pub fn render_payload(
        &self,
        variables: &Map<String, Value>,
        decision_task_id: &str,
        task_id: Option<&str>,
        input: Value,
    ) -> Result<Value> {
        let template = self
            .hook_payload
            .as_ref()
            .context("action has no hookPayload")?;
        let mut context = variables.clone();
        context.insert(
            "taskGroupId".to_string(),
            Value::String(decision_task_id.to_string()),
        );
        context.insert(
            "taskId".to_string(),
            task_id.map_or(Value::Null, |id| Value::String(id.to_string())),
        );
        context.insert("input".to_string(), input);
        json_e::render(template, &Value::Object(context))
    }
}

/// Returns the id of the created action task. `fix_hint` says how to fix
/// rejected or insufficient credentials.
pub async fn trigger(
    hooks: &taskcluster::Hooks,
    action: &Action,
    payload: &Value,
    fix_hint: &str,
) -> Result<String> {
    let (hook_group_id, hook_id) = action.hook()?;
    let response = hooks
        .triggerHook(hook_group_id, hook_id, payload)
        .await
        .map_err(|err| trigger_error(err, fix_hint, hook_group_id, hook_id))?;

    response["status"]["taskId"]
        .as_str()
        .map(str::to_owned)
        .context("hook response has no status.taskId")
}

fn trigger_error(
    err: anyhow::Error,
    fix_hint: &str,
    hook_group_id: &str,
    hook_id: &str,
) -> anyhow::Error {
    match taskcluster::err_status_code(&err) {
        Some(StatusCode::UNAUTHORIZED) => anyhow!(
            "Taskcluster rejected the credentials (expired, deleted or disabled). {fix_hint}"
        ),
        Some(StatusCode::FORBIDDEN) => {
            anyhow!("Insufficient scopes to trigger {hook_group_id}/{hook_id}. {fix_hint}")
        }
        // A 4xx response means the hook did not fire.
        Some(status) if status.is_client_error() => {
            anyhow!("Taskcluster refused to trigger {hook_group_id}/{hook_id}: {err:#}")
        }
        _ => anyhow!(
            "Triggering {hook_group_id}/{hook_id} failed: {err:#}. The request may or may not have reached Taskcluster; check the push before re-running."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ACTIONS_TRY: &str = include_str!("../tests/fixtures/actions_try.json");
    const TASK_TAGS_TRY: &str = include_str!("../tests/fixtures/task_tags_try.json");
    const DECISION_TASK: &str = "XaZ3nv77QrmgvzciUTkKyA";
    const TASK: &str = "Dl6nuq_PQcKoBuNDTa0Jgw";

    fn actions() -> ActionsJson {
        parse_actions(ACTIONS_TRY).unwrap()
    }

    #[test]
    fn decision_scope_uses_the_exact_hook_and_accepts_a_matching_scope_prefix() {
        let json = actions();
        let required = json
            .find("cancel-all", None)
            .unwrap()
            .decision_scope()
            .unwrap();
        assert_eq!(
            required,
            "in-tree:hook-action:project-gecko/in-tree-action-1-generic/b936906e1a"
        );
        assert!(scope_satisfies(
            "in-tree:hook-action:project-gecko/in-tree-action-1-*",
            &required
        ));
        assert!(!scope_satisfies(
            "in-tree:hook-action:project-gecko/in-tree-action-3-*",
            &required
        ));
        assert!(!scope_satisfies("hooks:trigger-hook:*", &required));
    }

    fn task_tags() -> HashMap<String, String> {
        serde_json::from_str(TASK_TAGS_TRY).unwrap()
    }

    #[test]
    fn finds_actions_without_context_for_the_push() {
        let actions = actions();
        for name in ["cancel-all", "add-new-jobs", "retrigger-multiple"] {
            assert_eq!(actions.find(name, None).unwrap().name, name);
        }
    }

    #[test]
    fn action_with_context_needs_a_matching_task() {
        let actions = actions();
        assert!(actions.find("cancel", None).is_err());
        assert_eq!(
            actions.find("cancel", Some(&task_tags())).unwrap().name,
            "cancel"
        );
    }

    #[test]
    fn retrigger_resolves_to_first_applicable_entry() {
        let actions = actions();
        let retrigger = actions.find("retrigger", Some(&task_tags())).unwrap();
        assert_eq!(
            retrigger.context,
            vec![HashMap::from([(
                "retrigger".to_string(),
                "true".to_string()
            )])]
        );

        // An empty tag-set matches any task, so it is the only entry for a bare task.
        let any_task = actions.find("retrigger", Some(&HashMap::new())).unwrap();
        assert_eq!(any_task.context, vec![HashMap::new()]);
    }

    #[test]
    fn unknown_action_lists_available_names() {
        let err = actions()
            .find("no-such-action", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no action named 'no-such-action'"));
        assert!(err.contains("cancel-all"));
        assert!(err.contains("retrigger-multiple"));
    }

    #[test]
    fn level_is_one_for_fixture_actions() {
        for action in &actions().actions {
            assert_eq!(action.level().unwrap(), 1);
        }
    }

    #[test]
    fn level_of_named_actions_is_known_without_a_target() {
        let actions = actions();
        assert_eq!(actions.level(&["cancel", "cancel-all"]).unwrap(), 1);
        assert!(actions
            .level(&["no-such-action"])
            .unwrap_err()
            .to_string()
            .contains("no action named 'no-such-action'"));
    }

    #[test]
    fn batch_results_must_match_the_requested_tasks() {
        let requested = BTreeSet::from(["a", "b"]);
        assert_eq!(
            match_requested(
                &requested,
                vec!["b".into(), "x".into(), "a".into()],
                String::as_str
            )
            .unwrap(),
            vec!["b".to_string(), "a".to_string()]
        );
        assert!(
            match_requested(&requested, vec!["a".into()], String::as_str)
                .unwrap_err()
                .to_string()
                .contains("no task b")
        );
    }

    #[test]
    fn level_parses_hook_id() {
        let mut action: Action = serde_json::from_value(json!({
            "name": "x",
            "kind": "hook",
            "context": [],
            "hookId": "in-tree-action-3-generic/abc"
        }))
        .unwrap();
        assert_eq!(action.level().unwrap(), 3);
        action.hook_id = Some("in-tree-action-2-generic/abc".into());
        assert!(action
            .level()
            .unwrap_err()
            .to_string()
            .contains("SCM level 2 are unsupported"));
    }

    #[test]
    fn render_payload_places_task_and_input() {
        let actions = actions();
        let tags = task_tags();

        let cancel_all = actions
            .find("cancel-all", None)
            .unwrap()
            .render_payload(&actions.variables, DECISION_TASK, None, json!({}))
            .unwrap();
        assert_eq!(cancel_all["decision"]["action"]["name"], "cancel-all");
        assert_eq!(cancel_all["user"]["taskGroupId"], DECISION_TASK);
        assert!(cancel_all["user"]["taskId"].is_null());
        assert_eq!(cancel_all["user"]["input"], json!({}));

        let cancel = actions
            .find("cancel", Some(&tags))
            .unwrap()
            .render_payload(&actions.variables, DECISION_TASK, Some(TASK), json!({}))
            .unwrap();
        assert_eq!(cancel["decision"]["action"]["name"], "cancel");
        assert_eq!(cancel["user"]["taskGroupId"], DECISION_TASK);
        assert_eq!(cancel["user"]["taskId"], TASK);
        assert_eq!(cancel["user"]["input"], json!({}));

        let input = json!({
            "requests": [{"tasks": ["test-linux/opt-mochitest-plain-1"], "times": 2}]
        });
        let retrigger = actions
            .find("retrigger-multiple", None)
            .unwrap()
            .render_payload(&actions.variables, DECISION_TASK, None, input.clone())
            .unwrap();
        assert_eq!(
            retrigger["decision"]["action"]["name"],
            "retrigger-multiple"
        );
        assert_eq!(retrigger["user"]["input"], input);
    }

    #[test]
    fn explicit_context_keys_win_over_variables() {
        let action: Action = serde_json::from_value(json!({
            "name": "x",
            "kind": "hook",
            "context": [],
            "hookPayload": {
                "task": {"$eval": "taskId"},
                "group": {"$eval": "taskGroupId"},
                "var": {"$eval": "other"}
            }
        }))
        .unwrap();
        let variables = json!({"taskId": "stale", "taskGroupId": "stale", "other": "kept"});
        let rendered = action
            .render_payload(
                variables.as_object().unwrap(),
                DECISION_TASK,
                None,
                Value::Null,
            )
            .unwrap();
        assert_eq!(
            rendered,
            json!({"task": null, "group": DECISION_TASK, "var": "kept"})
        );
    }

    #[test]
    fn cache_root_prefers_xdg_cache_home() {
        let root = tempfile::tempdir().unwrap();
        let xdg = root.path().join("xdg");
        assert_eq!(
            cache_root_from_env(Some(xdg.clone()), Some(root.path().join("native"))).unwrap(),
            xdg.join("treeherder-cli").join("taskcluster-actions")
        );
    }

    #[test]
    fn cache_root_falls_back_to_platform_cache() {
        let root = tempfile::tempdir().unwrap();
        let platform = root.path().join("native");
        assert_eq!(
            cache_root_from_env(None, Some(platform.clone())).unwrap(),
            platform.join("treeherder-cli").join("taskcluster-actions")
        );
        assert_eq!(
            cache_root_from_env(Some("relative".into()), Some(platform.clone())).unwrap(),
            platform.join("treeherder-cli").join("taskcluster-actions")
        );
    }

    #[test]
    fn cache_root_is_optional() {
        assert!(cache_root_from_env(None, None).is_none());
    }

    #[test]
    fn prune_removes_only_entries_older_than_thirty_days() {
        let root = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let dir = root.path().join("actions");
        let stale = dir.join("old.json");
        let fresh = dir.join("fresh.json");
        let unrelated = root.path().join("unrelated.json");
        let temporary = dir.join(".temporary");
        for path in [&stale, &unrelated, &temporary] {
            write_atomic(path, b"old", false).unwrap();
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(now - CACHE_MAX_AGE - Duration::from_secs(60))
                .unwrap();
        }
        assert!(read_cache(&stale).is_none());
        write_cache(&fresh, b"first");
        write_cache(&fresh, b"replacement");

        assert!(!stale.exists());
        assert_eq!(read_cache(&fresh).as_deref(), Some("replacement"));
        assert!(unrelated.exists());
        assert!(temporary.exists());
    }
}
