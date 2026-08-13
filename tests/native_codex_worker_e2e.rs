#![cfg(target_os = "linux")]

use ember::agent::{
    AgentProvider, AgentRuntimeManager, AgentSessionOutcome, CodexAppServerExitCause,
    CodexAppServerPhase, CreateWorktreeRequest, NativePromptPolicy, NewTask,
    SemanticCommandContext, TaskId, TaskManager, TaskStatus, TaskValidationStatus, WorktreeService,
};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

const POLICY: NativePromptPolicy = NativePromptPolicy {
    share_command_context: true,
    redact_secrets: true,
};
const REQUIRE_E2E_ENV: &str = "EMBER_REQUIRE_NATIVE_CODEX_E2E";

// Keep this fixture pinned to the same audited 0.147 config surface as the
// production adapter. A production policy change must deliberately update
// this independent wire peer instead of silently weakening the E2E proof.
const DISABLED_FEATURES: &[&str] = &[
    "apps",
    "auth_elicitation",
    "browser_use",
    "browser_use_external",
    "browser_use_full_cdp_access",
    "code_mode",
    "code_mode_host",
    "code_mode_only",
    "computer_use",
    "enable_mcp_apps",
    "external_agent_memory_import",
    "goals",
    "guardian_approval",
    "hooks",
    "image_generation",
    "in_app_browser",
    "in_app_updates",
    "memories",
    "mentions_v2",
    "multi_agent",
    "plugin_sharing",
    "plugins",
    "recommended_plugins",
    "remote_control",
    "remote_plugin",
    "shell_snapshot",
    "skill_mcp_dependency_install",
    "skill_search",
    "tool_call_mcp_elicitation",
    "tool_suggest",
    "view_image",
    "workspace_dependencies",
];

struct PrivateTestRoot(PathBuf);

impl PrivateTestRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "ember-native-worker-e2e-{}",
            Uuid::new_v4().simple()
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .expect("create private E2E root");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateTestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Process environment is global, so this integration binary intentionally
/// contains one test. Restore every changed value after the runtime has joined
/// its worker and released the provider process.
struct EnvironmentGuard(Vec<(String, Option<OsString>)>);

impl EnvironmentGuard {
    fn configure(bin: &Path, source_codex_home: &Path) -> Self {
        let changes = [
            ("PATH", Some(bin.as_os_str().to_os_string())),
            (
                "CODEX_HOME",
                Some(source_codex_home.as_os_str().to_os_string()),
            ),
            ("HOME", Some(source_codex_home.as_os_str().to_os_string())),
            ("LANG", None),
            ("LC_ALL", None),
            ("LC_CTYPE", None),
            ("LOGNAME", None),
            ("TZ", None),
            ("USER", None),
        ];
        let mut saved = Vec::with_capacity(changes.len());
        for (name, value) in changes {
            saved.push((name.to_string(), std::env::var_os(name)));
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        Self(saved)
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        for (name, value) in self.0.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

fn unsupported_reason() -> Option<String> {
    let systemd_run = Path::new("/usr/bin/systemd-run");
    if !systemd_run.is_file()
        || fs::metadata(systemd_run)
            .map(|metadata| metadata.permissions().mode() & 0o111 == 0)
            .unwrap_or(true)
    {
        return Some("/usr/bin/systemd-run is unavailable".into());
    }
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none_or(|value| value.is_empty()) {
        return Some("DBUS_SESSION_BUS_ADDRESS is unavailable".into());
    }
    let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
    else {
        return Some("XDG_RUNTIME_DIR is unavailable".into());
    };
    let bus = runtime_dir.join("bus");
    if UnixStream::connect(&bus).is_err() {
        return Some("the user D-Bus socket is unavailable".into());
    }
    if !Path::new("/sys/fs/cgroup/cgroup.controllers").is_file() {
        return Some("the host does not expose a unified cgroup v2 mount".into());
    }
    let Ok(cgroup) = fs::read_to_string("/proc/self/cgroup") else {
        return Some("/proc/self/cgroup is unavailable".into());
    };
    let Some(relative) = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .filter(|path| path.starts_with('/'))
    else {
        return Some("the process is not attached to a unified cgroup".into());
    };
    let current = Path::new("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
    if !current.join("cgroup.events").is_file() || !current.join("cgroup.kill").exists() {
        return Some("the current unified cgroup lacks events/kill support".into());
    }
    None
}

fn checked_git(cwd: &Path, arguments: &[&str]) {
    let status = Command::new("/usr/bin/git")
        .current_dir(cwd)
        .args(arguments)
        .status()
        .expect("run fixture git");
    assert!(status.success(), "git {arguments:?} failed with {status}");
}

fn create_managed_worktree(root: &Path) -> ember::agent::ManagedWorktree {
    let repository = root.join("repository");
    fs::create_dir(&repository).expect("create fixture repository");
    checked_git(&repository, &["init", "--quiet"]);
    fs::write(repository.join("tracked.txt"), b"baseline\n").expect("write fixture file");
    checked_git(&repository, &["add", "--", "tracked.txt"]);
    checked_git(
        &repository,
        &[
            "-c",
            "user.name=Ember E2E",
            "-c",
            "user.email=ember-e2e@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "baseline",
        ],
    );
    WorktreeService::new(root.join("managed"))
        .expect("create managed worktree service")
        .create(&CreateWorktreeRequest::new(
            &repository,
            "native-worker",
            "ember/native-worker-e2e",
            "HEAD",
        ))
        .expect("create managed E2E worktree")
}

fn create_source_codex_home(root: &Path) -> PathBuf {
    let home = root.join("source-codex-home");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .expect("create source Codex home");
    let auth = home.join("auth.json");
    fs::write(
        &auth,
        br#"{"auth_mode":"chatgpt","tokens":{"access_token":"native-e2e-access-token","account_id":"native-e2e-account","refresh_token":"must-not-cross-boundary"}}"#,
    )
    .expect("write source auth fixture");
    fs::set_permissions(&auth, fs::Permissions::from_mode(0o600))
        .expect("protect source auth fixture");
    home
}

fn create_trusted_codex_fixture(root: &Path) -> PathBuf {
    let bin = root.join("trusted-bin");
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&bin)
        .expect("create trusted fixture bin");
    let codex = bin.join("codex");
    fs::copy("/bin/sh", &codex).expect("copy trusted ELF shell fixture");
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o700))
        .expect("protect trusted fixture executable");
    bin
}

fn expected_tool_environment(bin: &Path, source_codex_home: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "HOME".to_string(),
            source_codex_home.to_string_lossy().into_owned(),
        ),
        ("PATH".to_string(), bin.to_string_lossy().into_owned()),
    ])
}

fn attested_config(tool_environment: &BTreeMap<String, String>) -> Value {
    let features: Map<String, Value> = DISABLED_FEATURES
        .iter()
        .map(|feature| ((*feature).to_string(), Value::Bool(false)))
        .collect();
    let tool_environment_json: Map<String, Value> = tool_environment
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect();
    let mut origins: Map<String, Value> = DISABLED_FEATURES
        .iter()
        .map(|feature| {
            (
                format!("features.{feature}"),
                json!({"name": {"type": "sessionFlags"}, "version": "e2e"}),
            )
        })
        .collect();
    for field in [
        "web_search",
        "allow_login_shell",
        "shell_environment_policy.inherit",
        "shell_environment_policy.ignore_default_excludes",
    ] {
        origins.insert(
            field.into(),
            json!({"name": {"type": "sessionFlags"}, "version": "e2e"}),
        );
    }
    for name in tool_environment.keys() {
        origins.insert(
            format!("shell_environment_policy.set.{name}"),
            json!({"name": {"type": "sessionFlags"}, "version": "e2e"}),
        );
    }

    let shell_policy = json!({
        "inherit": "none",
        "ignore_default_excludes": false,
        "set": tool_environment_json,
    });
    json!({
        "config": {
            "mcp_servers": {},
            "plugins": {},
            "marketplaces": {},
            "agents": null,
            "apps": null,
            "default_permissions": null,
            "hooks": null,
            "notify": null,
            "orchestrator": null,
            "permissions": null,
            "features": features,
            "web_search": "disabled",
            "allow_login_shell": false,
            "shell_environment_policy": shell_policy,
        },
        "origins": origins,
        "layers": [
            {
                "name": {"type": "sessionFlags", "version": "e2e"},
                "version": "e2e",
                "config": {
                    "features": features,
                    "web_search": "disabled",
                    "allow_login_shell": false,
                    "shell_environment_policy": shell_policy,
                },
            },
            {
                "name": {
                    "type": "user",
                    "file": "__EMBER_CODEX_HOME__/config.toml",
                    "profile": null,
                },
                "version": "e2e",
                "config": {},
            },
            {
                "name": {"type": "system", "file": "/etc/codex/config.toml"},
                "version": "e2e",
                "config": {},
            },
        ],
    })
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_app_server_fixture(worktree: &Path, tool_environment: &BTreeMap<String, String>) {
    let config_response = serde_json::to_string(&json!({
        "id": 2,
        "result": attested_config(tool_environment),
    }))
    .expect("serialize config response");
    let (config_before_home, config_after_home) = config_response
        .split_once("__EMBER_CODEX_HOME__")
        .expect("config response contains dynamic private home marker");
    let script = format!(
        r#"#!/bin/sh
set -eu

expected_disabled=' apps auth_elicitation browser_use browser_use_external browser_use_full_cdp_access code_mode code_mode_host code_mode_only computer_use enable_mcp_apps external_agent_memory_import goals guardian_approval hooks image_generation in_app_browser in_app_updates memories mentions_v2 multi_agent plugin_sharing plugins recommended_plugins remote_control remote_plugin shell_snapshot skill_mcp_dependency_install skill_search tool_call_mcp_elicitation tool_suggest view_image workspace_dependencies '
seen_disabled=' '
seen_strict=0
seen_web=0
seen_login=0
seen_inherit=0
seen_excludes=0
seen_stdio=0
while [ "$#" -gt 0 ]; do
    case "$1" in
        --strict-config) seen_strict=1; shift ;;
        --disable)
            [ "$#" -ge 2 ] || exit 70
            case "$expected_disabled" in
                *" $2 "*) seen_disabled="${{seen_disabled}}$2 " ;;
                *) printf 'unexpected disabled feature: %s\n' "$2" >&2; exit 71 ;;
            esac
            shift 2
            ;;
        -c)
            [ "$#" -ge 2 ] || exit 72
            case "$2" in
                'web_search="disabled"') seen_web=1 ;;
                'allow_login_shell=false') seen_login=1 ;;
                'shell_environment_policy.inherit="none"') seen_inherit=1 ;;
                'shell_environment_policy.ignore_default_excludes=false') seen_excludes=1 ;;
                shell_environment_policy.set.*) ;;
                *) printf 'unexpected config override: %s\n' "$2" >&2; exit 73 ;;
            esac
            shift 2
            ;;
        --stdio) seen_stdio=1; shift ;;
        *) printf 'unexpected app-server argument: %s\n' "$1" >&2; exit 74 ;;
    esac
done
[ "$seen_disabled" = "$expected_disabled" ] || {{ printf 'incomplete disabled features: %s\n' "$seen_disabled" >&2; exit 75; }}
[ "$seen_strict$seen_web$seen_login$seen_inherit$seen_excludes$seen_stdio" = 111111 ] || exit 76

read_wire() {{
    stage="$1"
    if ! IFS= read -r wire; then
        printf 'fixture EOF while waiting for %s\n' "$stage" >&2
        exit 80
    fi
}}

mismatch() {{
    printf 'fixture wire mismatch at %s: %s\n' "$1" "$wire" >&2
    exit "$2"
}}

read_wire initialize
case "$wire" in
    *'"id":1'*'"method":"initialize"'*) ;;
    *) mismatch initialize 81 ;;
esac
printf '%s%s%s\n' '{{"id":1,"result":{{"userAgent":"ember/0.147.0 (native-worker-e2e)","codexHome":"' "$CODEX_HOME" '"}}}}'

read_wire initialized
case "$wire" in
    '{{"method":"initialized"}}') ;;
    *) mismatch initialized 82 ;;
esac

read_wire config-read
case "$wire" in
    *'"id":2'*'"method":"config/read"'*'"includeLayers":true'*) ;;
    *) mismatch config-read 83 ;;
esac
printf '%s%s%s\n' {config_before_home} "$CODEX_HOME" {config_after_home}

read_wire external-login
case "$wire" in
    *'"id":3'*'"method":"account/login/start"'*'"accessToken":"native-e2e-access-token"'*'"chatgptAccountId":"native-e2e-account"'*) ;;
    *) mismatch external-login 84 ;;
esac
case "$wire" in
    *'must-not-cross-boundary'*|*'refreshToken'*|*'refresh_token'*) mismatch refresh-token-leak 84 ;;
esac
printf '%s\n' '{{"id":3,"result":{{"type":"chatgptAuthTokens"}}}}'

read_wire thread-start
case "$wire" in
    *'"id":4'*'"method":"thread/start"'*'"sessionStartSource":"startup"'*) ;;
    *) mismatch thread-start 85 ;;
esac
printf '%s\n' '{{"id":4,"result":{{"thread":{{"id":"thread-native-e2e"}}}}}}'

read_wire turn-one
case "$wire" in
    *'"id":5'*'"method":"turn/start"'*) ;;
    *) mismatch turn-one 86 ;;
esac
case "$wire" in
    *'"threadId":"thread-native-e2e"'*) ;;
    *) mismatch turn-one-thread 86 ;;
esac
case "$wire" in
    *'native-worker-turn-one-marker'*) ;;
    *) mismatch turn-one-prompt 86 ;;
esac
printf '%s\n' '{{"id":5,"result":{{"turn":{{"id":"provider-turn-1"}}}}}}'
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"thread-native-e2e","turnId":"provider-turn-1","itemId":"agent-message-1","delta":"first native E2E answer"}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"thread-native-e2e","turn":{{"id":"provider-turn-1","status":"completed"}}}}}}'

read_wire turn-two
case "$wire" in
    *'"id":6'*'"method":"turn/start"'*) ;;
    *) mismatch turn-two 87 ;;
esac
case "$wire" in
    *'"threadId":"thread-native-e2e"'*) ;;
    *) mismatch turn-two-thread 87 ;;
esac
case "$wire" in
    *'second-turn-e2e-marker'*) ;;
    *) mismatch turn-two-prompt 87 ;;
esac
printf '%s\n' '{{"id":6,"result":{{"turn":{{"id":"provider-turn-2"}}}}}}'
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"thread-native-e2e","turnId":"provider-turn-2","itemId":"agent-message-2","delta":"second native E2E answer"}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"thread-native-e2e","turn":{{"id":"provider-turn-2","status":"completed"}}}}}}'

# FinishSession is an Ember-local command and must not create another wire
# request. EOF is expected only after the worker accepts Finish and closes the
# real provider stdin during contained shutdown.
if IFS= read -r wire; then
    mismatch unexpected-after-turn-two 88
fi
exit 0
"#,
        config_before_home = shell_quote(config_before_home),
        config_after_home = shell_quote(config_after_home),
    );
    let fixture = worktree.join("app-server");
    fs::write(&fixture, script).expect("write app-server fixture");
    fs::set_permissions(&fixture, fs::Permissions::from_mode(0o700))
        .expect("protect app-server fixture");
}

fn register_task(tasks: &mut TaskManager, worktree: &ember::agent::ManagedWorktree) -> TaskId {
    tasks
        .create(NewTask {
            title: "native worker clean E2E".into(),
            provider: AgentProvider::Codex,
            repo_root: worktree.repository.clone(),
            worktree_path: worktree.path.clone(),
            branch: worktree.branch.clone(),
            base_commit: worktree.head.clone(),
            source_context: Some(SemanticCommandContext {
                source_session_id: "native-worker-e2e-source".into(),
                source_execution_id: "native-worker-e2e-execution".into(),
                source_sequence: 1,
                source_shell: Some("/bin/sh".into()),
                command: Some("false # native-worker-turn-one-marker".into()),
                command_exact: true,
                command_truncated: false,
                cwd: Some(worktree.repository.to_string_lossy().into_owned()),
                cwd_after: Some(worktree.repository.to_string_lossy().into_owned()),
                exit_code: Some(1),
                duration_ms: Some(10),
                output_text: "native worker fixture failure".into(),
                output_available: true,
                output_truncated: false,
                output_total_bytes: 29,
                started_at: None,
                finished_at: None,
            }),
        })
        .expect("register native E2E task")
}

fn panic_if_runtime_failed(runtime: &AgentRuntimeManager, tasks: &TaskManager, task_id: TaskId) {
    let task = tasks.get(task_id).expect("E2E task remains registered");
    if matches!(task.status, TaskStatus::Failed | TaskStatus::Cancelled) {
        panic!(
            "native E2E runtime stopped early: status={:?}, detail={:?}, exit={:?}, view={:?}",
            task.status,
            task.status_detail,
            runtime.exit_report(task_id),
            runtime.snapshot(task_id)
        );
    }
}

fn wait_for_review_turn(
    runtime: &mut AgentRuntimeManager,
    tasks: &mut TaskManager,
    task_id: TaskId,
    completed_turns: usize,
) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let report = runtime.poll(tasks, POLICY);
        assert!(
            report.issues.is_empty(),
            "native E2E runtime issues: {:?}",
            report.issues
        );
        panic_if_runtime_failed(runtime, tasks, task_id);
        let task = tasks.get(task_id).expect("E2E task remains registered");
        let ready = task.status == TaskStatus::ReadyForReview
            && tasks.has_active_agent_event_stream(task_id)
            && runtime.snapshot(task_id).is_some_and(|view| {
                view.phase == CodexAppServerPhase::Ready && view.completed_turns == completed_turns
            });
        if ready {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for native review turn {completed_turns}: task={task:?}, view={:?}, exit={:?}",
            runtime.snapshot(task_id),
            runtime.exit_report(task_id)
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn real_worker_two_turns_finish_reap_and_unlock_validation() {
    if let Some(reason) = unsupported_reason() {
        if std::env::var_os(REQUIRE_E2E_ENV).as_deref() == Some(std::ffi::OsStr::new("1")) {
            panic!("required native Codex worker E2E prerequisites are unavailable: {reason}");
        }
        eprintln!("SKIP native Codex worker E2E: {reason}");
        return;
    }

    let root = PrivateTestRoot::new();
    let managed = create_managed_worktree(root.path());
    let source_codex_home = create_source_codex_home(root.path());
    let bin = create_trusted_codex_fixture(root.path());
    let tool_environment = expected_tool_environment(&bin, &source_codex_home);
    write_app_server_fixture(&managed.path, &tool_environment);

    let _environment = EnvironmentGuard::configure(&bin, &source_codex_home);
    let mut runtime = AgentRuntimeManager::new();
    let mut tasks = TaskManager::new();
    let task_id = register_task(&mut tasks, &managed);

    runtime
        .start_codex(&mut tasks, task_id, POLICY)
        .expect("queue production native preparation");
    wait_for_review_turn(&mut runtime, &mut tasks, task_id, 1);

    runtime
        .prompt_codex(
            &tasks,
            task_id,
            "complete second-turn-e2e-marker on the same thread",
            POLICY,
        )
        .expect("send production follow-up turn");
    wait_for_review_turn(&mut runtime, &mut tasks, task_id, 2);
    let live_view = runtime
        .snapshot(task_id)
        .expect("native E2E retains its two-turn presentation");
    assert_eq!(live_view.turn_history.len(), 1);
    assert_eq!(live_view.turn_history[0].ordinal, 1);
    assert_eq!(
        live_view.turn_history[0].agent_text,
        "first native E2E answer"
    );
    assert!(live_view.turn_history[0].follow_up_feedback.is_none());
    assert_eq!(live_view.displayed_turn_ordinal, Some(2));
    assert_eq!(
        live_view.displayed_follow_up_feedback.as_deref(),
        Some("complete second-turn-e2e-marker on the same thread")
    );
    assert_eq!(live_view.agent_text, "second native E2E answer");
    assert_ne!(
        live_view.turn_history[0].local_turn_id,
        live_view
            .displayed_turn_id
            .expect("latest flat turn has a local identity")
    );
    assert!(tasks.has_active_agent_event_stream(task_id));
    assert!(tasks.next_validation_attempt(task_id).is_err());

    runtime
        .finish_codex(&tasks, task_id)
        .expect("finish idle production native session");
    let deadline = Instant::now() + Duration::from_secs(30);
    while runtime.has_running(task_id) || runtime.exit_report(task_id).is_none() {
        let report = runtime.poll(&mut tasks, POLICY);
        assert!(
            report.issues.is_empty(),
            "native E2E finish issues: {:?}",
            report.issues
        );
        assert!(
            Instant::now() < deadline,
            "timed out waiting for contained native Finish: task={:?}, view={:?}, exit={:?}",
            tasks.get(task_id),
            runtime.snapshot(task_id),
            runtime.exit_report(task_id)
        );
        thread::sleep(Duration::from_millis(10));
    }

    let exit = runtime
        .exit_report(task_id)
        .expect("runtime retains authoritative exit report");
    assert_eq!(exit.outcome, AgentSessionOutcome::Clean);
    assert_eq!(exit.cause, CodexAppServerExitCause::Clean);
    assert!(exit.process.spawned);
    assert!(exit.process.provider_released);
    assert!(exit.process.reaped);
    assert!(exit.process.containment_verified_empty);
    assert!(!runtime.can_continue_in_terminal(task_id));

    let task = tasks.get(task_id).expect("E2E task remains registered");
    assert_eq!(task.status, TaskStatus::ReadyForReview);
    assert!(!tasks.has_active_agent_event_stream(task_id));
    assert_eq!(tasks.next_validation_attempt(task_id), Ok(1));
    tasks
        .bind_validation_session(task_id, "native-clean-e2e-validation".into())
        .expect("clean contained Finish unlocks validation");
    assert_eq!(
        tasks.get(task_id).unwrap().validation.status,
        TaskValidationStatus::Running
    );
}
