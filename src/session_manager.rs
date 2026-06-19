use crate::session::Session;
use crate::session_persistence;
use crate::shell::ShellSession;
use crate::terminal::{clamp_terminal_dimensions, TerminalState};
use eframe::egui;
use parking_lot::Mutex as ParkingMutex;
use std::sync::Arc;

/// 获取指定进程的工作目录
pub fn get_process_cwd(pid: i32) -> Option<String> {
    // 从 /proc/[pid]/cwd 获取指定进程的工作目录
    std::fs::read_link(format!("/proc/{}/cwd", pid))
        .ok()
        .and_then(|path| path.to_str().map(|s| s.to_string()))
}

/// SessionManager - 管理所有终端会话
pub struct SessionManager {
    sessions: Vec<Session>,
    active_index: usize,
    repaint_ctx: egui::Context,
    configured_shell: Option<String>,
}

impl SessionManager {
    /// 创建新的会话管理器，初始化一个默认会话
    pub fn new(first_session: Session, repaint_ctx: egui::Context, configured_shell: Option<String>) -> Self {
        SessionManager {
            sessions: vec![first_session],
            active_index: 0,
            repaint_ctx,
            configured_shell,
        }
    }

    /// 创建新会话并添加到当前活跃会话的右侧，继承当前工作目录
    pub fn new_session(
        &mut self,
        name: Option<String>,
        tags: Option<Vec<String>>,
        cols: usize,
        rows: usize,
        scrollback_lines: usize,
    ) -> usize {
        let (cols, rows) = clamp_terminal_dimensions(cols, rows);
        let insert_index = self.active_index + 1;
        let name = name.unwrap_or_else(|| format!("Session {}", self.sessions.len() + 1));
        let tags = tags.unwrap_or_default();

        // 优先使用 shell 通过 OSC 7 报告的 cwd(SSH/tmux 等场景下 /proc 不能反
        // 映远端进程真实目录);否则退回 /proc/[pid]/cwd。
        let cwd = if !self.sessions.is_empty() {
            let active_session = &self.sessions[self.active_index];
            let osc7 = active_session
                .terminal
                .lock()
                .current_working_dir
                .clone();
            osc7.or_else(|| get_process_cwd(active_session.get_shell_pid()))
        } else {
            None
        };

        // 创建新会话，继承工作目录（新会话不传 session_id，自动生成）
        let cwd_ref = cwd.as_deref();
        match ShellSession::new_with_cwd(cols, rows, cwd_ref, None, self.configured_shell.as_deref(), self.repaint_ctx.clone()) {
            Ok(shell) => {
                let mut terminal = TerminalState::new(cols, rows);
                terminal.set_max_scrollback(scrollback_lines);
                let terminal = Arc::new(ParkingMutex::new(terminal));
                let session = Session::new(name, tags, terminal, shell);
                self.sessions.insert(insert_index, session);
                insert_index
            }
            Err(e) => {
                eprintln!("Failed to create new session: {}", e);
                self.active_index
            }
        }
    }

    /// 关闭指定会话
    pub fn close_session(&mut self, index: usize) -> bool {
        if index >= self.sessions.len() {
            return false;
        }

        if self.sessions.len() == 1 {
            // 不允许关闭最后一个会话
            return false;
        }

        self.sessions.remove(index);

        // 调整活跃会话索引:
        // - 关闭的是活跃会话之前的会话:活跃会话整体左移一位,索引需 -1 才能继续指向同一会话。
        // - 关闭的就是活跃会话:索引保持不变,自然指向原先的下一个会话(下方再做越界钳制)。
        if index < self.active_index {
            self.active_index -= 1;
        }
        if self.active_index >= self.sessions.len() {
            self.active_index = self.sessions.len() - 1;
        }

        true
    }

    /// 切换到指定会话
    pub fn switch_session(&mut self, index: usize) -> bool {
        if index < self.sessions.len() {
            self.active_index = index;
            if let Some(session) = self.sessions.get_mut(index) {
                session.metadata.update_last_active();
            }
            true
        } else {
            false
        }
    }

    /// 切换到下一个会话
    pub fn switch_to_next_session(&mut self) -> usize {
        self.active_index = (self.active_index + 1) % self.sessions.len();
        self.active_index
    }

    /// 切换到前一个会话
    pub fn switch_to_prev_session(&mut self) -> usize {
        if self.active_index == 0 {
            self.active_index = self.sessions.len() - 1;
        } else {
            self.active_index -= 1;
        }
        self.active_index
    }

    /// 获取当前活跃会话的索引
    pub fn active_index(&self) -> usize {
        self.active_index
    }

    /// 获取当前活跃会话（可变引用）
    pub fn get_active_session_mut(&mut self) -> &mut Session {
        &mut self.sessions[self.active_index]
    }

    /// 获取指定索引的会话（可变引用）
    pub fn get_session_mut(&mut self, index: usize) -> Option<&mut Session> {
        self.sessions.get_mut(index)
    }

    /// 获取所有会话的不可变引用
    pub fn sessions(&self) -> &[Session] {
        &self.sessions
    }

    /// 获取所有会话的可变引用
    pub fn sessions_mut(&mut self) -> &mut [Session] {
        &mut self.sessions
    }

    /// 会话总数（始终 ≥ 1，不存在空状态，故无 is_empty）
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// 重排会话顺序（拖拽）
    pub fn reorder_sessions(&mut self, from_idx: usize, to_idx: usize) {
        if from_idx < self.sessions.len() && to_idx < self.sessions.len() && from_idx != to_idx {
            let session = self.sessions.remove(from_idx);
            self.sessions.insert(to_idx, session);

            // 如果移动的是活跃会话，更新active_index
            if self.active_index == from_idx {
                self.active_index = to_idx;
            } else if from_idx < self.active_index && to_idx >= self.active_index {
                // 从左边移到右边，active_index向左移动
                self.active_index -= 1;
            } else if from_idx > self.active_index && to_idx <= self.active_index {
                // 从右边移到左边，active_index向右移动
                self.active_index += 1;
            }
        }
    }

    /// 获取会话列表的快照用于持久化（包含 cwd）
    pub fn get_session_snapshots(&self) -> Vec<session_persistence::SessionSnapshot> {
        self.sessions
            .iter()
            .map(|s| {
                let cwd = get_process_cwd(s.get_shell_pid());
                session_persistence::SessionSnapshot {
                    name: s.metadata.name.clone(),
                    tags: s.metadata.tags.clone(),
                    cwd,
                    session_id: Some(s.metadata.session_id.clone()),
                }
            })
            .collect()
    }

    /// 从快照恢复额外的会话（第一个已经在外部创建好）
    pub fn restore_from_snapshots(
        &mut self,
        snapshots: Vec<session_persistence::SessionSnapshot>,
        active_index: Option<usize>,
    ) {
        // 用第一个快照的 name/tags/session_id 更新已有的第一个 session
        if let Some(first) = snapshots.first() {
            if let Some(session) = self.sessions.get_mut(0) {
                session.metadata.name = first.name.clone();
                session.metadata.tags = first.tags.clone();
                if let Some(ref sid) = first.session_id {
                    session.metadata.session_id = sid.clone();
                }
            }
        }

        // 为剩余快照创建新会话
        for snap in snapshots.into_iter().skip(1) {
            let cwd_ref = snap.cwd.as_deref();
            let sid_ref = snap.session_id.as_deref();
            match ShellSession::new_with_cwd(80, 24, cwd_ref, sid_ref, self.configured_shell.as_deref(), self.repaint_ctx.clone()) {
                Ok(shell) => {
                    let terminal = Arc::new(ParkingMutex::new(TerminalState::new(80, 24)));
                    let mut session = Session::new(snap.name, snap.tags, terminal, shell);
                    if let Some(sid) = snap.session_id {
                        session.metadata.session_id = sid;
                    }
                    self.sessions.push(session);
                }
                Err(e) => {
                    eprintln!("Failed to restore session: {}", e);
                }
            }
        }

        // 恢复活跃标签页
        if let Some(idx) = active_index {
            if idx < self.sessions.len() {
                self.active_index = idx;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // 注意: 完整的单元测试需要创建真实的 TerminalState 和 ShellSession
    // 这里只测试基本逻辑
}
