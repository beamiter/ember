# Ember: Agent-Native Terminal Action Plan

> Status: Working product/architecture roadmap
>
> Goal: evolve Ember from a fast GPU-accelerated Linux terminal emulator into an **agent-native execution environment** where humans, shells, and coding agents share the same structured task context.

## 1. Product thesis

Traditional terminals are optimized around **processes, panes, and byte streams**. AI coding tools introduce a different unit of work: the **task**.

Ember should not become another IDE with an AI sidebar, and it should not try to clone Orca feature-for-feature. Its strongest position is lower in the stack:

> **Traditional terminals manage processes. Ember manages work completed by humans and agents together.**

The target category is:

**Ember — The Agent-Native Terminal**

Core promise:

- keep the full power and compatibility of a real terminal;
- understand shell execution semantically rather than as opaque scrollback;
- let Codex, Claude Code, OpenCode, and future agents become first-class runtime backends;
- turn failed commands, diffs, tests, benchmarks, and approvals into structured task objects;
- preserve human control through explicit capabilities and provenance;
- make multiple agents easy to run, compare, evaluate, and resume.

A concise homepage message could be:

> **Run commands. Delegate failures. Race agents. Review results.**

## 2. Why Ember has a credible starting advantage

Ember already owns the layer that most AI coding products treat as a black box: the terminal execution surface.

Existing foundations that should be reused rather than replaced:

- independent PTY-backed shell sessions;
- tabs and nested split panes;
- WGPU/egui rendering;
- per-pane working directory;
- git branch / dirty state;
- running-command state;
- OSC 133 semantic command blocks and completed command history;
- long-running command notifications;
- session persistence;
- file sidebar;
- remote SSH/container sessions;
- bounded PTY input/output queues and protocol safety controls;
- existing agent-aware input safety primitives such as guarded agent input and foreground-PTY checks.

These features mean Ember can build AI interaction from **real execution state** instead of screen scraping.

## 3. What changes for the user

### 3.1 Traditional terminal workflow

```text
human types command
      ↓
program emits bytes
      ↓
human reads output
      ↓
human recognizes failure
      ↓
human copies context
      ↓
human opens Claude/Codex
      ↓
human explains cwd / command / error / repo state
      ↓
agent proposes changes
      ↓
human returns to terminal
      ↓
human validates result
```

### 3.2 Ember target workflow

```text
cargo test
    ↓
    ✗
    ↓
[Fix] [Explain] [Retry] [Create Agent Task]
    ↓
Codex / Claude / Race Both
    ↓
structured task context
    ↓
file changes + commands + tests
    ↓
review diff
    ↓
validated result
```

The key UX shift is that the user stops manually moving context between tools.

## 4. Signature UX: Command → Agent Task

This should be the first feature that makes Ember visibly different from Kitty, Ghostty, Alacritty, and ordinary terminal + agent combinations.

After a semantic command block finishes with an error:

```text
──────────────────────────────────────────────
✗ cargo test                         8.42s
exit 101 · 7 errors

[Fix] [Explain] [Retry] [Create Agent Task]
──────────────────────────────────────────────
```

Selecting **Fix** opens:

```text
Fix with

[ Codex ] [ Claude ] [ Race Both ]
```

Ember constructs context from structured state, not arbitrary recent scrollback:

```text
cwd
repo
branch
git status
command
exit code
stdout/stderr for this command block
related previous command blocks
selection, if explicitly attached
relevant files, when known
```

The resulting agent task should remain linked to the command that created it.

### Acceptance criteria

- one click from a failed command block to an agent task;
- no copy/paste required;
- task knows source pane, cwd, command, exit code, and exact semantic output block;
- agent result can automatically rerun the originating command;
- user can review the final diff before accepting changes.

## 5. Do not make the Agent UI another terminal TUI

The terminal grid should remain optimized for terminal programs. Agent turns should use native egui components.

Bad target:

```text
$ claude
╭──────────────────────╮
│ another TUI in a PTY │
╰──────────────────────╯
```

Preferred target:

```text
┌─ Codex · Fix PTY hang ───────────────────────┐
│ Plan                                         │
│ ✓ inspect PTY lifecycle                      │
│ ✓ modify src/pty.rs                          │
│ ● cargo test                                 │
│                                              │
│ Changed                                      │
│ src/pty.rs                         +18 -7     │
│ src/session.rs                      +6 -3     │
│                                              │
│ Validation                                   │
│ ✓ cargo fmt                                  │
│ ● cargo test                                 │
│                                              │
│ [Review Diff] [Stop]                         │
└──────────────────────────────────────────────┘
```

The main unit becomes the **turn/result**, not ANSI characters.

## 6. Core architecture

```text
                         ┌─────────────────────┐
                         │      Ember UI       │
                         │ terminal / agent /  │
                         │ diff / tasks        │
                         └─────────┬───────────┘
                                   │
                         Semantic Event Bus
                                   │
                ┌──────────────────┼──────────────────┐
                │                  │                  │
         Workspace Context      ActionGate       Task Manager
         cwd/git/commands       approvals        worktrees/jobs
                │                  │                  │
                └──────────────────┼──────────────────┘
                                   │
                         Agent Driver Interface
                           /               \
                          /                 \
                    Codex Driver       Claude Driver
                         │                  │
                    app-server       stream-json / SDK

                         Ember MCP Server
                           /          \
                      Codex MCP    Claude MCP
```

### 6.1 Pane backend separation

Do not keep growing `ShellSession` into a universal runtime.

Target model:

```rust
enum PaneBackend {
    Terminal(TerminalSession),
    Agent(AgentSession),
}

enum PaneSurface {
    Terminal(TerminalSurface),
    Agent(AgentSurface),
    Diff(DiffSurface),
}
```

Longer-term ownership:

```text
EmberApp
  └─ Workspace
      └─ Task / Pane
          └─ Surface
```

## 7. Provider-independent AgentDriver

UI code must not depend directly on Codex or Claude protocol details.

Suggested normalized event model:

```rust
pub enum AgentEvent {
    TurnStarted,

    TextDelta(String),
    ReasoningDelta(String),
    PlanUpdated(Plan),

    CommandStarted {
        id: String,
        command: String,
        cwd: PathBuf,
    },
    CommandOutput {
        id: String,
        data: Vec<u8>,
    },
    CommandFinished {
        id: String,
        exit_code: i32,
    },

    FileChanged(FileChange),
    DiffUpdated(Diff),

    ApprovalRequested(ApprovalRequest),
    PermissionRequested(PermissionRequest),

    ToolStarted(ToolCall),
    ToolFinished(ToolResult),

    UsageUpdated(TokenUsage),

    TurnCompleted,
    Error(AgentError),
}
```

Suggested driver interface:

```rust
#[async_trait]
pub trait AgentDriver {
    async fn start(&mut self, ctx: AgentContext) -> Result<AgentId>;
    async fn prompt(&mut self, prompt: Prompt) -> Result<()>;
    async fn steer(&mut self, message: String) -> Result<()>;
    async fn cancel(&mut self) -> Result<()>;
    async fn approve(
        &mut self,
        request: ApprovalId,
        decision: ApprovalDecision,
    ) -> Result<()>;
    async fn resume(
        &mut self,
        session: ProviderSessionId,
    ) -> Result<()>;
}
```

### 7.1 Codex integration

Prefer **Codex app-server** for the native path rather than parsing its TUI.

Why:

- structured thread / turn / item lifecycle;
- streamed events;
- command execution events;
- file-change events;
- approvals and permission requests;
- resumable conversations;
- cleaner native UI mapping.

Keep a compatibility path that can still launch the normal Codex CLI in a PTY.

### 7.2 Claude Code integration

Initial native path:

```text
claude
  --input-format stream-json
  --output-format stream-json
  --include-partial-messages
  --include-hook-events
```

Use Claude hooks / structured events for lifecycle and permission integration rather than scraping its screen.

Later, evaluate a Claude Agent SDK sidecar if it offers a cleaner long-lived transport.

### 7.3 Provider strategy

Codex and Claude should be adapters, not architectural dependencies.

Future providers should be able to implement `AgentDriver` without changing the rest of Ember:

- OpenCode;
- Gemini CLI;
- local models;
- custom internal agents.

## 8. Semantic Terminal Context

This is one of Ember's strongest differentiators.

A conventional terminal exposes recent text. Ember should expose typed execution objects.

Suggested command record:

```rust
struct CommandExecution {
    id: CommandId,
    pane: PaneId,
    cwd: PathBuf,
    command: String,
    started_at: Timestamp,
    finished_at: Option<Timestamp>,
    exit_code: Option<i32>,
    output: CommandOutputRef,
    git_before: Option<GitSnapshot>,
    git_after: Option<GitSnapshot>,
    origin: ExecutionOrigin,
}
```

Context references should be explicit:

```text
@pane:1
@block:42
@last-error
@selection
@file:src/session.rs
@git-diff
@branch
@workspace
```

Example composer:

```text
╭────────────────────────────────────────────────────╮
│ Ask Codex...                               Ctrl+↵  │
│ @pane:1  @last-error  @selection                   │
╰────────────────────────────────────────────────────╯
```

Benefits:

- less token waste;
- less accidental disclosure;
- more reproducible prompts;
- stronger provenance;
- no need to guess which scrollback matters.

## 9. Ember MCP server

Create an `ember-mcp` process so agents can interact with Ember semantically.

Suggested read-only tools:

```text
ember.get_workspace
ember.list_panes
ember.get_pane_context
ember.get_selection
ember.list_command_blocks
ember.get_command_block
ember.get_current_command
ember.get_last_failed_command
```

Suggested UI/navigation tools:

```text
ember.open_file
ember.reveal_file
ember.notify
```

Suggested execution tools:

```text
ember.propose_command
ember.wait_for_command
```

Avoid a generic unrestricted `send_keys` capability as the primary execution API.

The agent should request intent:

```text
ember.propose_command(
    pane = 3,
    command = "cargo test --release"
)
```

Ember then decides whether the action can run.

## 10. ActionGate: Ember as the final local authority

Worktrees do not protect the whole machine. An agent can still touch home-directory secrets, Docker, SSH, network services, or production systems.

Ember should therefore keep a capability layer outside provider-specific permission systems.

Suggested policy surface:

```text
CODEX permissions

Filesystem
✓ task worktree          read/write
✓ repository             read
✗ ~/.ssh                 denied
✗ ~/.aws                 denied

Commands
✓ cargo *
✓ git status/diff
✓ rg
? git push               ask
? docker                 ask
✗ sudo                   deny

Network
✓ crates.io
? arbitrary network      ask
```

Approval card example:

```text
Codex wants to execute

git push origin fix-pty

Reason
Publish tested branch

[Once] [Task] [Always] [Deny]
```

Rules:

- provider sandbox/approval systems remain enabled;
- Ember adds an outer capability boundary;
- agent-originated PTY input must keep the existing foreground-shell safety requirement;
- command provenance must survive approvals and retries.

## 11. Worktree tasks: the best lesson from Orca

Orca's strongest product insight is treating a task/worktree as the main isolation and workflow unit.

Ember should adopt this without turning into another full IDE.

Target sidebar:

```text
WORKSPACE
ember

TASKS
├─ ● fix resize crash
│   ├─ Codex
│   ├─ Terminal
│   └─ +21 -7
│
├─ ● optimize renderer
│   ├─ Claude
│   └─ +84 -32
│
└─ ✓ OSC clipboard fix

TERMINALS
├─ ~/ember
└─ gpu01
```

A task can own:

```text
Task
 ├─ metadata
 ├─ git worktree
 ├─ branch
 ├─ one or more AgentSessions
 ├─ terminal sessions
 ├─ diff state
 ├─ validation commands
 └─ execution graph
```

Lifecycle:

```text
Create
  ↓
Work
  ↓
Validate
  ↓
Review
  ↓
Commit / Merge / PR
  ↓
Archive
```

Important: preserve ordinary terminal mode. Ember should support both **Terminal Mode** and **Task Mode**.

## 12. Agent dashboard: show who needs the human

When multiple agents are active, users should not need to inspect tabs individually.

Target task list:

```text
TASKS

● Renderer crash                    CODEX
  Testing · cargo test renderer
  01:28

● Vulkan optimization               CLAUDE
  Editing renderer.rs
  +83 -21

! OSC bug                           CODEX
  Waiting for approval

✓ startup performance               CLAUDE
  tests passed
```

Agent states should normalize into something like:

```rust
enum AgentActivity {
    Starting,
    Thinking,
    Editing,
    RunningCommand,
    WaitingForApproval,
    WaitingForHuman,
    Finished,
    Failed,
}
```

The dashboard should answer one question immediately:

> **Who needs me now?**

## 13. Diff becomes a first-class surface

Do not build a full IDE editor first. Build an excellent review surface.

Required capabilities:

- combined staged / unstaged / untracked view;
- file-level and hunk-level review;
- per-line or per-hunk comments;
- send review comments back to the originating agent;
- show which agent turn produced a change;
- image diff later;
- conflict visualization later.

Example:

```text
┌─ Codex · Fix PTY hang ───────────────────────┐
│ src/pty.rs                         +18 -7     │
│                                              │
│ 218 │ pollfd.events = POLLIN;                │
│-219 │ read(fd, ...)                          │
│+219 │ match retry_on_eintr(...) {            │
│                                              │
│ 💬 This branch still looks racy              │
│                                              │
│ [Send review to Codex]                       │
└──────────────────────────────────────────────┘
```

## 14. Execution Graph: Ember's deeper moat

Orca is primarily organized around worktree → diff. Ember can go deeper because it owns command execution.

Every meaningful event should be linkable:

```text
                    TASK
                     │
        ┌────────────┼────────────┐
        │            │            │
      Human        Codex        Claude
        │            │            │
        ▼            ▼            ▼
 Command #31     Agent Turn    Agent Turn
 cargo test           │
      ✗               │
        └──────┬──────┘
               ▼
          File changes
               │
               ▼
          cargo test
               ✓
               │
               ▼
          cargo clippy
               ✓
               │
               ▼
             Diff
               │
               ▼
             Commit
```

Suggested provenance type:

```rust
enum ExecutionOrigin {
    Human,
    Agent {
        provider: AgentProvider,
        session: ProviderSessionId,
        turn: AgentTurnId,
    },
    Automation {
        task: TaskId,
    },
}
```

Questions Ember should be able to answer:

- who ran this command?
- why was it run?
- which task and agent turn caused it?
- which files changed before/after it?
- which test validated the change?
- which commit contains the result?

This turns terminal history into an **execution history with causality**.

## 15. Agent Race: do not merely parallelize — evaluate

Orca demonstrates the value of running several agents in isolated worktrees. Ember should extend that idea with executable evaluation.

Flow:

```text
                Task
                 │
        ┌────────┼────────┐
        ▼        ▼        ▼
      Codex    Claude   OpenCode
        │        │        │
       WT1      WT2      WT3
        │        │        │
        └────────┼────────┘
                 ▼
             Evaluator
                 │
        ┌────────┼────────┐
        ▼        ▼        ▼
       tests  benchmark  lint
                 │
                 ▼
             ranking
```

Example result:

```text
TASK: Optimize startup

Baseline       47.8 ms

Codex #1       42.1 ms
Codex #2       39.7 ms   ★
Claude #1      41.3 ms
Claude #2      failed

Best           -17.0%

[Review winner]
```

Evaluator inputs may include:

- tests;
- lint;
- benchmark latency;
- memory use;
- binary size;
- FPS;
- GPU utilization;
- custom scripts.

This is a natural terminal-native extension because the terminal already owns execution and measurement.

## 16. What not to build yet

Avoid losing focus by cloning IDE surfaces that do not strengthen Ember's core moat.

Low priority for now:

- full Monaco-like editor;
- integrated general-purpose browser;
- issue tracker clone;
- giant AI chat sidebar;
- proprietary model integration tied to one vendor;
- unrestricted agent `send_keys` APIs;
- UI features that cannot be represented in the semantic event model.

Ember should be best at:

```text
Terminal          ★★★★★
Agent Runtime     ★★★★★
Tasks             ★★★★★
Execution Graph   ★★★★★
Diff / Review     ★★★★☆
Worktrees         ★★★★☆
Editor            ★☆☆☆☆
Browser           ★☆☆☆☆
```

## 17. Delivery roadmap

### P0 — Useful AI terminal immediately

Goal: achieve the most valuable part of the Orca experience with minimal architectural disruption.

Deliverables:

1. agent launcher for Codex / Claude / OpenCode;
2. normalized agent status: working / waiting / done / failed;
3. task sidebar;
4. one-task-one-worktree helper;
5. native diff surface;
6. agent completion / approval notifications;
7. attach a PTY agent session to a task.

Exit condition:

> Ember is already a good multi-agent terminal even before native provider protocols are complete.

### P1 — Ember-specific differentiation

Goal: stop treating agents as opaque PTY applications.

Deliverables:

1. `AgentDriver` abstraction — implemented for the first Codex vertical slice;
2. `AgentEvent` normalization — implemented with stream/turn correlation;
3. Codex app-server driver — live sequential-turn MVP implemented;
   native prerequisites are prepared asynchronously with cancellation and
   generation/policy stale-result rejection, followed by descriptor-backed Git
   and launcher revalidation in the provider worker immediately before spawn;
   completed turns remain on the same loaded thread for bounded review feedback,
   with a 32-turn identity-retention cap; an explicit clean finish is required
   before validation and stopped sessions cannot yet resume. The initial view is
   current/latest-turn only, with bounded completed-turn history still pending;
4. Claude stream-json driver;
5. native Agent Pane — initial Tasks-dashboard projection implemented;
6. **Fix this command** — implemented for exact failed-command evidence;
7. **Explain this command**;
8. context references: `@last-error`, `@block`, `@pane`, `@selection`, `@git-diff`;
9. `ember-mcp` read/context tools;
10. ActionGate and native approval cards — exact display-and-deny cards implemented;
    all native approvals remain non-accepting until granted actions can be
    bound to the pinned worktree and containment boundary;
11. provider session resume after Ember restart — not yet implemented.

Exit condition:

> A user can move from a failed terminal command to a structured, resumable native agent task without copying text or interacting with the provider TUI.

### P2 — Execution-native workflow

Goal: make Ember understand the causality of development work.

Deliverables:

1. Execution Graph storage;
2. human / agent / automation command provenance;
3. structured command cards;
4. Agent Turn ↔ command ↔ file-change links;
5. validation command groups;
6. task summary/result cards;
7. review comments sent back to the correct agent turn;
8. task-level persistence and archive.

Exit condition:

> Ember can explain what happened during a task, who/what caused each important action, and how the final result was validated.

### P3 — Multi-agent evaluation engine

Goal: turn multiple agents into an optimization workflow, not a collection of tabs.

Deliverables:

1. agent race orchestration;
2. isolated worktrees per candidate;
3. evaluator definitions;
4. automatic test/lint/benchmark execution;
5. result comparison and ranking;
6. winner review/apply flow;
7. CLI support for scripted races.

Possible CLI shape:

```bash
ember task race \
  --agents codex,claude \
  --eval "cargo test && cargo bench"
```

Exit condition:

> Users can state an objective, let multiple agents explore independently, and let Ember evaluate candidates using executable criteria.

## 18. Suggested source layout

```text
src/
├── agent/
│   ├── mod.rs
│   ├── driver.rs
│   ├── event.rs
│   ├── session.rs
│   ├── context.rs
│   ├── approval.rs
│   ├── persistence.rs
│   └── drivers/
│       ├── codex.rs
│       ├── claude.rs
│       └── opencode.rs
│
├── task/
│   ├── mod.rs
│   ├── worktree.rs
│   ├── evaluator.rs
│   └── execution_graph.rs
│
├── ui/
│   ├── agent_pane.rs
│   ├── agent_turn.rs
│   ├── agent_command.rs
│   ├── agent_diff.rs
│   ├── task_sidebar.rs
│   └── approval_card.rs
│
└── ...

crates/
└── ember-mcp/
    └── ...
```

Do not treat this directory structure as mandatory; preserve existing Ember module boundaries where they remain cleaner.

## 19. Core data model

A minimal first version could revolve around these IDs and objects:

```text
WorkspaceId
TaskId
PaneId
CommandId
AgentSessionId
AgentTurnId
ProviderSessionId
ApprovalId
DiffId
EvaluationId
```

Relationships:

```text
Workspace
 ├─ Tasks
 └─ ordinary terminal sessions

Task
 ├─ Worktree
 ├─ AgentSessions
 ├─ TerminalSessions
 ├─ Commands
 ├─ Diffs
 ├─ Evaluations
 └─ ExecutionGraph

AgentSession
 └─ AgentTurns

AgentTurn
 ├─ ToolCalls
 ├─ Commands
 ├─ FileChanges
 ├─ Approvals
 └─ Usage
```

Store stable IDs so sessions can be resumed after restart and links remain valid after UI navigation changes.

## 20. Product principles

### 20.1 Terminal remains real

Do not weaken terminal compatibility to make AI easier. SSH, Vim, Neovim, Helix, Emacs, htop, tmux, arbitrary TUIs, and ordinary shells remain first-class.

### 20.2 Structured data beats screen scraping

Prefer app-server, stream-json, hooks, OSC metadata, MCP, git metadata, and direct process state over parsing visible characters.

### 20.3 Context is explicit

Agent context should be inspectable and attachable. Avoid silently sending an entire scrollback unless explicitly requested.

### 20.4 Actions have provenance

Every agent-originated command or file operation should be traceable to a task/session/turn.

### 20.5 Evaluation beats subjective comparison

When multiple agents produce candidates, prefer executable criteria before asking the human to inspect large diffs.

### 20.6 Vendor independence

Codex and Claude are important integrations, not Ember's identity.

### 20.7 Humans supervise tasks, not terminal tabs

The primary multi-agent interface should answer:

> What is running? What finished? What failed? Who needs approval? What result is ready to review?

## 21. First implementation slice

If only one end-to-end slice is implemented first, build this:

```text
cargo test
    ↓
    ✗
    ↓
Fix with Codex
    ↓
create task + capture semantic command context
    ↓
run Codex in isolated worktree
    ↓
show agent status
    ↓
show native diff
    ↓
running validation: cargo test
    ↓
    ✓
    ↓
Ready to review
```

This slice exercises almost every future architectural boundary while producing immediate user value.

Minimal dependencies for the slice:

1. `Task` object;
2. worktree creation;
3. command-block → context conversion;
4. Codex launcher initially, native driver later if necessary;
5. task status;
6. diff surface;
7. validation command;
8. result card.

## 22. Success metrics

Avoid judging progress primarily by number of AI features.

Measure workflow compression instead.

Candidate metrics:

- clicks/keystrokes from failed command to agent working;
- percentage of agent tasks created directly from command blocks;
- amount of manual context pasted into agents;
- time spent switching tabs to check agent status;
- percentage of tasks with automatic validation;
- percentage of agent commands with traceable provenance;
- percentage of multi-agent races automatically ranked by evaluator;
- task resume success after Ember restart;
- number of dangerous operations blocked/escalated by ActionGate;
- median time from failure to reviewed validated fix.

The strongest north-star metric could be:

> **Time from failed execution to validated reviewed result.**

## 23. Competitive framing

### Traditional terminals

Position: best-in-class character/process interfaces.

Ember advantage: semantic execution + agent task layer.

### AI-enhanced terminals

Common position: terminal plus chat/generation UI.

Ember advantage: agents are not a sidebar feature; they are first-class execution participants.

### Orca

Strongest ideas worth adopting:

- task/worktree as the primary coding unit;
- multiple agents per task;
- task lifecycle;
- excellent diff/review loop;
- visible agent status;
- parallel-agent workflows.

Where Ember should differentiate:

- remain terminal-native rather than becoming another IDE;
- native structured agent transports instead of PTY as the only source of truth;
- semantic command context from OSC 133 and shell execution;
- ActionGate outside provider sandboxes;
- command/process/test provenance;
- Execution Graph;
- executable multi-agent evaluation.

## 24. Decision summary

### Build

- Task abstraction;
- worktree lifecycle;
- native Agent Pane;
- provider-independent AgentDriver;
- Codex app-server integration;
- Claude structured stream integration;
- command-block actions;
- semantic context references;
- native diff/review;
- `ember-mcp`;
- ActionGate;
- provenance;
- Execution Graph;
- Agent Race + evaluator.

### Defer

- full IDE editor;
- general browser replacement;
- broad project-management suite;
- provider-specific architecture;
- unrestricted agent automation.

### Product identity

Do not optimize for:

> "terminal with AI"

Optimize for:

> **"the execution environment where humans and agents work together."**

---

## 25. Immediate next 10 engineering tickets

1. Introduce `TaskId`, `Task`, and `TaskStatus` without changing current terminal behavior.
2. Add task sidebar behind an experimental feature flag.
3. Implement create/delete/archive git worktree service with strict path validation.
4. Add `ExecutionOrigin` to semantic command records; default existing records to `Human`.
5. Add command-block actions: **Explain**, **Fix**, **Create Agent Task**.
6. Add a provider-neutral `AgentDriver` + `AgentEvent` skeleton with a fake/test driver.
7. Implement a basic Codex task adapter and attach its lifecycle to `Task`.
8. Implement a read-only native diff surface that can display the active task worktree diff.
9. Add task validation commands and a result card (`passed` / `failed` / `needs review`).
10. Prototype `ember-mcp` with read-only tools: workspace, panes, command blocks, last failed command.

After these ten tickets, revisit the architecture before adding more provider integrations.

## 26. Reference material

- Ember repository: <https://github.com/beamiter/ember>
- Codex app-server documentation: <https://developers.openai.com/codex/app-server/>
- Codex MCP documentation: <https://developers.openai.com/codex/mcp/>
- Claude Code CLI reference: <https://docs.anthropic.com/en/docs/claude-code/cli-reference>
- Claude Code hooks: <https://docs.anthropic.com/en/docs/claude-code/hooks>
- Orca: <https://github.com/stablyai/orca>
- Orca documentation: <https://www.onorca.dev/docs>

This document intentionally treats external provider protocols as replaceable adapters. Before implementation, re-check their current schemas and lifecycle guarantees because these interfaces evolve quickly.
