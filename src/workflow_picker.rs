//! 工作流选择器与参数填写对话框的纯状态层（Ctrl+Shift+M 打开）。
//!
//! 发现、读取、校验、渲染全部搬去了 `jterm_core::workflows`（迁移说明见
//! [`crate::workflows`]）；这里只剩 egui 真正需要的两样东西：浮层的
//! 查询/高亮/聚焦，以及参数对话框里 `TextEdit` 必须拿到的可变缓冲区。
//!
//! 查询/过滤/高亮状态机同样使用核心的 [`WorkflowPicker`]。egui 的
//! `text_edit_singleline` 必须拿到 `&mut String`，所以薄壳只保留一份编辑缓冲；
//! 每帧写回核心的 `set_query`，由它统一做单行过滤、UTF-8 安全截断和高亮复位。
//!
//! 参数模型换成核心的 [`ArgsForm`]：它把“文件没声明默认值、用户也没填”与
//! “用户主动填了空串”分开保存。旧实现把每个已声明参数都用空串预填，
//! `render` 的 missing-values 守卫因此在 ember 里永远不会触发——
//! `kill -9 {pid}` 空着提交会渲染成 `kill -9 ` 并回填提示符。现在这种提交
//! 报 `missing values: pid`，对话框保持打开，缺值的行在按 Enter 之前就带
//! 星号标记。
//!
//! 展示文本仍然一律经过 `visible_bounded`，但理由要说清楚：加载期确实已经
//! 拒绝了名称/描述/标签/命令里的控制与双向字符，所以转义那一半是纵深防御；
//! 真正不可省的是字节封顶——命令模板可以有 64 KiB，`missing values:` 可以
//! 把 64 个参数名串起来，单行 egui label 不该拿到这种长度。加上错误串是这
//! 个面上唯一没经过加载期校验的字符串，这一层保留。

use crate::workflows::{ArgsForm, Workflow, WorkflowArg};
use jterm_core::workflows::{PickerPolicy, WorkflowPicker};

/// 一次渲染/导航的最大结果数。与历史选择器一致：键盘选择与绘制共用
/// `filtered()`，上限同时约束两者。
pub const MAX_RESULTS: usize = 15;
const PICKER_POLICY: PickerPolicy = PickerPolicy::new(MAX_RESULTS, false);
/// 展示用名称/描述/命令预览的字节预算（仅影响显示，不影响回填文本）。
const MAX_LABEL_BYTES: usize = 256;
const MAX_PREVIEW_BYTES: usize = 4 * 1024;

/// 单行展示的安全截断：控制/隐形/双向字符显式转义，绝不带格式效应进 UI。
pub fn display_label(text: &str) -> String {
    crate::review_text::visible_bounded(text, MAX_LABEL_BYTES)
}

/// 对话框里的命令模板预览（等宽、可选中）。
pub fn display_command_preview(command: &str) -> String {
    crate::review_text::visible_bounded(command, MAX_PREVIEW_BYTES)
}

/// 工作流选择器状态。`entries` 在打开浮层时加载一次；之后磁盘上的新文件在
/// 下一次打开时出现（与历史选择器的加载语义一致）。
pub struct WorkflowPickerState {
    /// 是否需要聚焦搜索框（egui 文本框在浮层打开后的第一帧取焦）。
    pub needs_focus: bool,
    picker: WorkflowPicker,
    /// egui 的 `TextEdit` 所需可变缓冲。核心仍是查询的唯一真相；
    /// [`Self::sync_query`] 在编辑后立即规范化并写回。
    query_buffer: String,
}

impl WorkflowPickerState {
    /// 条目按拿到的顺序原样持有。顺序是加载策略的事，只说一次
    /// （[`crate::workflows::LOAD_ORDER`]）；旧实现在这里又按名字排了一遍，等于把
    /// 加载顺序悄悄覆盖掉——两处口径一旦不同，用户看到的是这一处。
    pub fn new(entries: Vec<Workflow>) -> Self {
        Self {
            needs_focus: true,
            picker: WorkflowPicker::new(entries, PICKER_POLICY),
            query_buffer: String::new(),
        }
    }

    pub fn query(&self) -> &str {
        self.picker.query()
    }

    /// 给 egui 一帧的编辑权限；调用方随后必须调用 [`Self::sync_query`]。
    pub fn query_buffer_mut(&mut self) -> &mut String {
        &mut self.query_buffer
    }

    /// 把 egui 缓冲写回共享状态机，并把核心的规范化结果同步回输入框。
    pub fn sync_query(&mut self) {
        if self.query_buffer == self.picker.query() {
            return;
        }
        let query = std::mem::take(&mut self.query_buffer);
        self.picker.set_query(query);
        self.query_buffer.push_str(self.picker.query());
    }

    pub fn selected(&self) -> usize {
        self.picker.selected()
    }

    pub fn select(&mut self, index: usize) -> bool {
        self.picker.select(index)
    }

    /// 当前过滤结果（最多 [`MAX_RESULTS`] 条），由共享状态机统一完成。
    pub fn filtered(&self) -> Vec<&Workflow> {
        self.picker.filtered()
    }

    /// 高亮项下移（在过滤结果中循环）。
    pub fn select_next(&mut self) {
        self.picker.select_next();
    }

    /// 高亮项上移（在过滤结果中循环）。
    pub fn select_prev(&mut self) {
        self.picker.select_prev();
    }

    /// 当前高亮的工作流（按过滤结果中的位置）。
    pub fn selected_workflow(&self) -> Option<&Workflow> {
        self.picker.selected_workflow()
    }
}

/// 参数填写对话框状态：核心 [`ArgsForm`] 的 egui 外壳。
pub struct WorkflowArgsState {
    form: ArgsForm,
    /// 每行的编辑缓冲。egui 的 `TextEdit` 要 `&mut String`，而 [`ArgsForm`]
    /// 刻意不外借内部值——Unset 与 Supplied("") 一旦被同一个 `&mut String`
    /// 抹平，缺值守卫就又没了。缓冲只是显示侧，[`Self::sync`] 是唯一写回
    /// 模型的入口。
    buffers: Vec<String>,
    /// 提交失败的错误信息（anvil 在同一对话框内显示错误并保持打开）。
    pub error: Option<String>,
    /// 是否需要聚焦第一个参数输入框。
    pub needs_focus: bool,
}

impl WorkflowArgsState {
    pub fn new(workflow: Workflow) -> Self {
        let form = ArgsForm::new(workflow);
        // 有默认值的行预填默认值，没有的行是空的——空在这里表示“还没填”，
        // 不表示“填了空串”。
        let buffers = (0..form.len())
            .map(|index| form.value(index).to_string())
            .collect();
        Self {
            form,
            buffers,
            error: None,
            needs_focus: true,
        }
    }

    pub fn workflow(&self) -> &Workflow {
        self.form.workflow()
    }

    /// 参数行数（= 声明的参数个数）。
    pub fn arg_count(&self) -> usize {
        self.form.len()
    }

    /// 一行的参数声明与它的编辑缓冲。一次取出两半，渲染循环就不必同时借
    /// `form` 和 `buffers`。
    pub fn row_mut(&mut self, index: usize) -> Option<(&WorkflowArg, &mut String)> {
        let arg = self.form.args().get(index)?;
        let buffer = self.buffers.get_mut(index)?;
        Some((arg, buffer))
    }

    /// 把缓冲写回模型：绘制完一帧后调用一次。
    ///
    /// 判据是内容而不是 egui 的 `changed()` 事件——模型与用户眼前的输入框
    /// 之间不能有第二个真相，一个漏掉的事件会让回填的命令和屏幕上的字不一
    /// 样。内容相同就不写：这样“从没动过、文件也没给默认值”的行保持 Unset，
    /// 缺值守卫才有东西可依据；一旦用户敲了字（哪怕后来又删空），这一行就
    /// 变成“用户提供的值”，空串能不能用由核心的 `render` 判定。
    pub fn sync(&mut self) {
        for index in 0..self.buffers.len() {
            if self.form.value(index) != self.buffers[index] {
                self.form.set(index, self.buffers[index].clone());
            }
        }
    }

    /// 这一行是否仍然缺值。规则本身在核心（没有声明默认值且当前为空），
    /// 这里只做下标转名字的查表，避免在 UI 侧复述一份判据。
    pub fn is_missing(&self, index: usize) -> bool {
        self.form
            .args()
            .get(index)
            .is_some_and(|arg| self.form.missing().contains(&arg.name.as_str()))
    }

    /// 用当前输入渲染命令模板。所有校验（值里的控制/隐形字符、缺失占位符、
    /// 渲染结果的 review-input 边界）都在核心的 `render` 里。
    pub fn render(&self) -> Result<String, String> {
        self.form.render()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn arg(name: &str, default: Option<&str>) -> WorkflowArg {
        WorkflowArg {
            name: name.to_string(),
            description: String::new(),
            default: default.map(str::to_string),
        }
    }

    #[test]
    fn empty_query_keeps_the_order_the_loader_chose() {
        // 加载顺序是 `crate::workflows::LOAD_ORDER` 的事：选择器不再重排，所以这里
        // 给的顺序原样出来（真实调用路径上，那个顺序已经是名称序）。
        let state = WorkflowPickerState::new(vec![
            workflow("zeta", "", &[]),
            workflow("alpha", "", &[]),
            workflow("mid", "", &[]),
        ]);
        let names: Vec<&str> = state.filtered().iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["zeta", "alpha", "mid"]);
    }

    #[test]
    fn fuzzy_query_matches_description_and_tags() {
        let mut state = WorkflowPickerState::new(vec![
            workflow("deploy", "Ship the service", &["ops"]),
            workflow("rebase", "Rewrite history", &["git"]),
        ]);
        *state.query_buffer_mut() = "ship".to_string();
        state.sync_query();
        let names: Vec<&str> = state.filtered().iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["deploy"]);

        *state.query_buffer_mut() = "git".to_string();
        state.sync_query();
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
        assert_eq!(state.picker.policy(), PICKER_POLICY);
        assert!(!state.picker.policy().search_command());

        state.select_prev();
        assert_eq!(state.selected(), MAX_RESULTS - 1);
        state.select_next();
        assert_eq!(state.selected(), 0);
        assert_eq!(
            state.selected_workflow().map(|w| w.name.as_str()),
            Some("wf-00")
        );
    }

    #[test]
    fn egui_query_buffer_crosses_the_shared_query_boundary() {
        let mut state = WorkflowPickerState::new(vec![workflow("alpha", "", &[])]);
        *state.query_buffer_mut() = format!(
            "{}\nignored",
            "x".repeat(jterm_core::workflows::MAX_PICKER_QUERY_BYTES + 16)
        );
        state.sync_query();
        assert!(state.query().len() <= jterm_core::workflows::MAX_PICKER_QUERY_BYTES);
        assert!(!state.query().contains('\n'));
        assert_eq!(state.query_buffer, state.query());
        assert_eq!(state.selected(), 0);
    }

    #[test]
    fn args_state_seeds_declared_defaults_and_renders_edits() {
        let mut wf = workflow("greet", "", &[]);
        wf.command = "echo hello {{who}} {{punct}}".to_string();
        wf.args = vec![arg("who", Some("world")), arg("punct", None)];
        let mut state = WorkflowArgsState::new(wf);

        // 声明了默认值的行预填；没声明的行是空的，而且是“还没填”。
        assert_eq!(state.row_mut(0).unwrap().1.as_str(), "world");
        assert_eq!(state.row_mut(1).unwrap().1.as_str(), "");
        assert!(!state.is_missing(0));
        assert!(state.is_missing(1));

        // 旧行为在这里返回 Ok("echo hello world ")——空串预填让守卫失效，
        // 一条半截命令直接回填到提示符。
        let error = state.render().unwrap_err();
        assert!(error.contains("missing values: punct"), "got {error}");

        *state.row_mut(1).unwrap().1 = "!".to_string();
        state.sync();
        assert!(!state.is_missing(1));
        assert_eq!(state.render().unwrap(), "echo hello world !");
    }

    #[test]
    fn emptying_a_defaulted_row_stays_a_deliberate_empty_value() {
        // 与上一条对称：文件声明了默认值，就等于声明了“空值在这里有意义”，
        // 所以清空这一行渲染成空，而不是回落到默认值、也不是缺值。
        let mut wf = workflow("deploy", "", &[]);
        wf.command = "deploy api --env={{env}}".to_string();
        wf.args = vec![arg("env", Some("staging"))];
        let mut state = WorkflowArgsState::new(wf);

        state.row_mut(0).unwrap().1.clear();
        state.sync();
        assert!(!state.is_missing(0));
        assert_eq!(state.render().unwrap(), "deploy api --env=");
    }

    #[test]
    fn args_state_rejects_unsafe_input() {
        let mut wf = workflow("unsafe", "", &[]);
        wf.command = "echo {value}".to_string();
        wf.args = vec![arg("value", None)];
        let mut state = WorkflowArgsState::new(wf);
        *state.row_mut(0).unwrap().1 = "ok\nrm -rf /".to_string();
        state.sync();
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
