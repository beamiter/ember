//! 工作流选择器与参数填写对话框的纯状态层（Ctrl+Shift+M 打开）。
//!
//! 语义移植自 anvil 的 command-palette 第三层（`:` 前缀 / `Action::OpenWorkflows`）
//! 与 `dialogs/workflow.rs` 参数对话框，UI 习惯沿用 ember 的
//! [`crate::history_picker`]：中央浮层 + 模糊匹配 + Enter 只回填提示符，绝不
//! 执行。与 anvil 的差异：anvil 按 source_path 回查工作流（它的缓存可能被后台
//! 刷新重建）；ember 在打开浮层时同步加载并直接持有 `Workflow` 克隆，路径回查
//! 不再需要。
//!
//! 展示文本一律经过 `visible_bounded`/`safe_inline_display`：工作流文件是磁盘
//! 上的不可信输入，即便加载期校验过控制字符，展示侧仍然独立设防。

use fuzzy_matcher::skim::SkimMatcherV2;
use fuzzy_matcher::FuzzyMatcher;
use std::collections::HashMap;

use crate::workflows::{self, Workflow};

/// 一次渲染/导航的最大结果数。与历史选择器一致：键盘选择与绘制共用
/// `filtered()`，上限同时约束两者。
pub const MAX_RESULTS: usize = 15;
/// 展示用名称/描述/命令预览的字节预算（仅影响显示，不影响回填文本）。
const MAX_LABEL_BYTES: usize = 256;
const MAX_PREVIEW_BYTES: usize = 4 * 1024;

/// 单行展示的安全截断：控制/隐形/双向字符显式转义，绝不带格式效应进 UI。
pub fn display_label(text: &str) -> String {
    crate::review_text::visible_bounded(text, MAX_LABEL_BYTES)
}

/// 对话框里的命令模板预览（等宽、可选中）。加载期校验保证无控制字符，
/// 这里再做一层显示侧转义，与历史选择器的 display_command 同一姿态。
pub fn display_command_preview(command: &str) -> String {
    crate::review_text::visible_bounded(command, MAX_PREVIEW_BYTES)
}

/// 工作流选择器状态。`entries` 在打开浮层时按名称排序加载一次；之后磁盘上
/// 的新文件在下一次打开时出现（与历史选择器的加载语义一致）。
pub struct WorkflowPickerState {
    pub query: String,
    /// 当前过滤结果中的高亮位置。
    pub selected: usize,
    /// 是否需要聚焦搜索框（egui 文本框在浮层打开后的第一帧取焦）。
    pub needs_focus: bool,
    entries: Vec<Workflow>,
    matcher: SkimMatcherV2,
}

impl WorkflowPickerState {
    pub fn new(mut entries: Vec<Workflow>) -> Self {
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Self {
            query: String::new(),
            selected: 0,
            needs_focus: true,
            entries,
            matcher: SkimMatcherV2::default(),
        }
    }

    /// 从给定目录加载（发现与校验的全部守卫都在 `workflows::load_all`）。
    pub fn load(dirs: &[std::path::PathBuf]) -> Self {
        Self::new(workflows::load_all(dirs))
    }

    /// 当前过滤结果（最多 [`MAX_RESULTS`] 条）。空查询保持名称序；否则按模糊
    /// 匹配分数降序，同分保持名称序（稳定排序）。名称、描述与标签一起参与
    /// 匹配——anvil 在 palette 里同样用标签辅助召回。
    pub fn filtered(&self) -> Vec<&Workflow> {
        if self.query.is_empty() {
            return self.entries.iter().take(MAX_RESULTS).collect();
        }
        let mut scored: Vec<(i64, &Workflow)> = self
            .entries
            .iter()
            .filter_map(|workflow| {
                let haystack = if workflow.tags.is_empty() {
                    format!("{} {}", workflow.name, workflow.description)
                } else {
                    format!(
                        "{} {} {}",
                        workflow.name,
                        workflow.description,
                        workflow.tags.join(" ")
                    )
                };
                self.matcher
                    .fuzzy_match(&haystack, &self.query)
                    .map(|score| (score, workflow))
            })
            .collect();
        scored.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        scored
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(_, workflow)| workflow)
            .collect()
    }

    /// 高亮项下移（在过滤结果中循环）。
    pub fn select_next(&mut self) {
        let len = self.filtered().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = (self.selected + 1) % len;
        }
    }

    /// 高亮项上移（在过滤结果中循环）。
    pub fn select_prev(&mut self) {
        let len = self.filtered().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = if self.selected == 0 {
                len - 1
            } else {
                self.selected - 1
            };
        }
    }

    /// 当前高亮的工作流（按过滤结果中的位置）。
    pub fn selected_workflow(&self) -> Option<&Workflow> {
        self.filtered().get(self.selected).copied()
    }
}

/// 参数填写对话框状态（anvil `dialogs/workflow.rs` 的对应物）。`values` 与
/// `workflow.args` 按下标对齐——egui 的 TextEdit 需要 `&mut String`，用
/// Vec 保持行序比 HashMap 更贴合 immediate-mode 的行渲染。
pub struct WorkflowArgsState {
    pub workflow: Workflow,
    pub values: Vec<String>,
    /// 提交失败的错误信息（anvil 在同一对话框内显示错误并保持打开）。
    pub error: Option<String>,
    /// 是否需要聚焦第一个参数输入框。
    pub needs_focus: bool,
}

impl WorkflowArgsState {
    pub fn new(workflow: Workflow) -> Self {
        let values = workflow
            .args
            .iter()
            .map(|arg| arg.default.clone().unwrap_or_default())
            .collect();
        Self {
            workflow,
            values,
            error: None,
            needs_focus: true,
        }
    }

    /// 用当前输入渲染命令模板。所有校验（值里的控制/隐形字符、缺失占位符、
    /// 渲染结果的 review-input 边界）都在 `workflows::render` 里。
    pub fn render(&self) -> Result<String, String> {
        let values: HashMap<String, String> = self
            .workflow
            .args
            .iter()
            .zip(self.values.iter())
            .map(|(arg, value)| (arg.name.clone(), value.clone()))
            .collect();
        workflows::render(&self.workflow, &values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::WorkflowArg;

    fn workflow(name: &str, description: &str, tags: &[&str]) -> Workflow {
        Workflow {
            name: name.to_string(),
            description: description.to_string(),
            command: "echo ok".to_string(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            shell: None,
            args: Vec::new(),
            source_path: None,
        }
    }

    #[test]
    fn empty_query_keeps_name_order() {
        let state = WorkflowPickerState::new(vec![
            workflow("zeta", "", &[]),
            workflow("alpha", "", &[]),
            workflow("mid", "", &[]),
        ]);
        let names: Vec<&str> = state.filtered().iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn fuzzy_query_matches_description_and_tags() {
        let mut state = WorkflowPickerState::new(vec![
            workflow("deploy", "Ship the service", &["ops"]),
            workflow("rebase", "Rewrite history", &["git"]),
        ]);
        state.query = "ship".to_string();
        let names: Vec<&str> = state.filtered().iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["deploy"]);

        state.query = "git".to_string();
        let names: Vec<&str> = state.filtered().iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["rebase"]);
    }

    #[test]
    fn results_are_capped_so_navigation_matches_the_drawn_list() {
        let entries = (0..MAX_RESULTS + 5)
            .map(|i| workflow(&format!("wf-{i:02}"), "", &[]))
            .collect();
        let mut state = WorkflowPickerState::new(entries);
        assert_eq!(state.filtered().len(), MAX_RESULTS);

        state.select_prev();
        assert_eq!(state.selected, MAX_RESULTS - 1);
        state.select_next();
        assert_eq!(state.selected, 0);
        assert_eq!(
            state.selected_workflow().map(|w| w.name.as_str()),
            Some("wf-00")
        );
    }

    #[test]
    fn args_state_seeds_defaults_and_renders() {
        let mut wf = workflow("greet", "", &[]);
        wf.command = "echo hello {{who}} {{punct}}!".to_string();
        wf.args = vec![
            WorkflowArg {
                name: "who".to_string(),
                description: String::new(),
                default: Some("world".to_string()),
            },
            WorkflowArg {
                name: "punct".to_string(),
                description: String::new(),
                default: None,
            },
        ];
        let mut state = WorkflowArgsState::new(wf);
        // 与 anvil 的对话框一致：无默认值的参数以空串预填，提交时按空值渲染
        // （"missing values" 只发生在调用方根本没有传入某个已声明参数时，
        // 对话框的种子值不会产生那种状态——该路径由 workflows::render 的
        // 单元测试覆盖）。
        assert_eq!(state.render().unwrap(), "echo hello world !");
        state.values[1] = "done".to_string();
        assert_eq!(state.render().unwrap(), "echo hello world done!");
    }

    #[test]
    fn args_state_rejects_unsafe_input() {
        let mut wf = workflow("unsafe", "", &[]);
        wf.command = "echo {value}".to_string();
        wf.args = vec![WorkflowArg {
            name: "value".to_string(),
            description: String::new(),
            default: None,
        }];
        let mut state = WorkflowArgsState::new(wf);
        state.values[0] = "ok\nrm -rf /".to_string();
        assert!(state
            .render()
            .unwrap_err()
            .contains("unsafe for review-only insertion"));
    }

    #[test]
    fn display_helpers_escape_spoofing_and_stay_bounded() {
        assert_eq!(display_label("safe\u{202e}txt"), "safe\\u{202E}txt");
        assert!(display_label(&"界".repeat(500)).len() <= MAX_LABEL_BYTES);
        assert_eq!(display_command_preview("echo\tx"), "echo\\tx");
        assert!(
            display_command_preview(&"x".repeat(MAX_PREVIEW_BYTES + 10)).len() <= MAX_PREVIEW_BYTES
        );
    }
}
