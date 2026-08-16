use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::atomic::{AtomicI32, Ordering};

pub const MAX_SESSION_SNAPSHOT_BYTES: u64 = 4 * 1024 * 1024;
pub const MAX_RESTORED_SESSIONS: usize = 64;
const MAX_SESSION_NAME_BYTES: usize = 256;
const MAX_SESSION_TAGS: usize = 32;
const MAX_SESSION_TAG_BYTES: usize = 128;
const MAX_SESSION_CWD_BYTES: usize = 4096;
const MAX_RESTORED_LAYOUT_DEPTH: usize = 64;
const MAX_RESTORED_LAYOUT_NODES: usize = MAX_RESTORED_SESSIONS * 2 - 1;
const INSTANCE_LOCK_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_millis(100);
const INSTANCE_LOCK_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(2);
const NO_INSTANCE_LOCK_FD: i32 = -1;
static INSTANCE_LOCK_FD: AtomicI32 = AtomicI32::new(NO_INSTANCE_LOCK_FD);

fn default_split_ratio() -> f32 {
    0.5
}

/// 持久化布局只引用稳定 session ID，不保存运行期的 session 数组索引。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutSnapshot {
    pub root: LayoutNodeSnapshot,
    #[serde(default)]
    pub focused_session_id: Option<String>,
    /// 固定的 tab 重启后仍固定。旧快照没有这个字段，恢复为未固定。
    #[serde(default)]
    pub pinned: bool,
    /// 同上，用于「重要」标记（多选模型）。
    #[serde(default)]
    pub marked: bool,
    /// Whether tab chrome should redact the real title.
    #[serde(default)]
    pub private_title: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LayoutNodeSnapshot {
    Pane {
        session_id: String,
    },
    Split {
        horizontal: bool,
        #[serde(default = "default_split_ratio")]
        ratio: f32,
        first: Box<LayoutNodeSnapshot>,
        second: Box<LayoutNodeSnapshot>,
    },
}

/// 布局损坏不应连带丢失整个 session 列表。先读为 Value，再单独尝试解析；
/// 失败时退化成 `None`，启动端会恢复为单 pane。
fn deserialize_optional_layout<'de, D>(deserializer: D) -> Result<Option<LayoutSnapshot>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}

/// 同上，用于 per-tab 布局列表：整段解析失败退回空列表，启动端会回落到
/// `layout` 字段（旧快照）或单 tab。
fn deserialize_tab_layouts<'de, D>(deserializer: D) -> Result<Vec<LayoutSnapshot>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// 会话持久化数据结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub name: String,
    pub tags: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    /// 用户在 tab 上双击重命名后的显示名;Some 时覆盖 CWD-derived 标题。
    #[serde(default)]
    pub custom_name: Option<String>,
}

/// 会话列表快照
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsSnapshot {
    pub version: u32,
    pub sessions: Vec<SessionSnapshot>,
    #[serde(default)]
    pub active_index: Option<usize>,
    /// 兼容字段：v3 及更早只有一棵全局布局树。写入时填当前 tab 的布局，
    /// 让旧版本仍能打开新快照；读取时只在 `tabs` 缺失时用于迁移。
    #[serde(default, deserialize_with = "deserialize_optional_layout")]
    pub layout: Option<LayoutSnapshot>,
    /// v4：每个 tab 一棵布局树。窗格归 tab 所有，因此布局也必须按 tab 存。
    #[serde(default, deserialize_with = "deserialize_tab_layouts")]
    pub tabs: Vec<LayoutSnapshot>,
    #[serde(default)]
    pub active_tab: Option<usize>,
}

// ---------------------------------------------------------------------------
// Schema-aware bounded snapshot decoding
// ---------------------------------------------------------------------------

const MAX_RESTORED_TABS: usize = MAX_RESTORED_SESSIONS;
const MAX_SESSION_ID_BYTES: usize = 128;
const MAX_LEGAL_RESTORED_TEXT_BYTES: usize = MAX_RESTORED_SESSIONS
    * (MAX_SESSION_NAME_BYTES
        + MAX_SESSION_TAGS * MAX_SESSION_TAG_BYTES
        + MAX_SESSION_CWD_BYTES
        + MAX_SESSION_ID_BYTES
        + MAX_SESSION_NAME_BYTES)
    + MAX_RESTORED_LAYOUT_NODES * MAX_SESSION_ID_BYTES
    + MAX_RESTORED_TABS * MAX_SESSION_ID_BYTES;
/// Maximum owned text retained by one decoded snapshot. The formula above is
/// the auditable sum of every legal field; the fixed ceiling leaves a small
/// independent margin while making format growth fail the compile-time check.
const MAX_RESTORED_TEXT_BYTES: usize = 600 * 1024;
const _: () = assert!(MAX_LEGAL_RESTORED_TEXT_BYTES <= MAX_RESTORED_TEXT_BYTES);

/// State shared by the bounded decoder. The 4 MiB input cap alone is not
/// enough: ordinary derived deserialization can still turn a compact JSON
/// array into thousands of owned elements before `sanitize` gets a chance to
/// truncate them. Every nested payload is therefore borrowed as a
/// [`serde_json::value::RawValue`] slice of the input and only decoded — under
/// these budgets — by a short-lived parser that is finished and dropped before
/// the decoder follows any of its children.
#[derive(Clone, Copy)]
struct DecodeBudget {
    text: jterm_core::bounded_json::TextBudget,
    remaining_layout_nodes: usize,
    repaired_fields: usize,
    extra_sessions: usize,
    layout_repaired: bool,
    active_tab_repaired: bool,
}

impl DecodeBudget {
    fn new(text_bytes: usize) -> Self {
        Self {
            text: jterm_core::bounded_json::TextBudget::new(text_bytes),
            remaining_layout_nodes: MAX_RESTORED_LAYOUT_NODES,
            repaired_fields: 0,
            extra_sessions: 0,
            layout_repaired: false,
            active_tab_repaired: false,
        }
    }

    fn charge_text<E: serde::de::Error>(
        &mut self,
        field: &'static str,
        bytes: usize,
    ) -> Result<(), E> {
        self.text.charge(field, bytes)
    }
}

#[derive(Clone, Copy)]
enum SnapshotField {
    Version,
    Sessions,
    ActiveIndex,
    Layout,
    Tabs,
    ActiveTab,
    Name,
    Tags,
    Cwd,
    SessionId,
    CustomName,
    Kind,
    Horizontal,
    Ratio,
    First,
    Second,
    Root,
    FocusedSessionId,
    Pinned,
    Marked,
    PrivateTitle,
    Unknown,
}

impl<'de> Deserialize<'de> for SnapshotField {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_identifier(SnapshotFieldVisitor)
    }
}

struct SnapshotFieldVisitor;

impl serde::de::Visitor<'_> for SnapshotFieldVisitor {
    type Value = SnapshotField;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a session snapshot field name")
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(match value {
            "version" => SnapshotField::Version,
            "sessions" => SnapshotField::Sessions,
            "active_index" => SnapshotField::ActiveIndex,
            "layout" => SnapshotField::Layout,
            "tabs" => SnapshotField::Tabs,
            "active_tab" => SnapshotField::ActiveTab,
            "name" => SnapshotField::Name,
            "tags" => SnapshotField::Tags,
            "cwd" => SnapshotField::Cwd,
            "session_id" => SnapshotField::SessionId,
            "custom_name" => SnapshotField::CustomName,
            "kind" => SnapshotField::Kind,
            "horizontal" => SnapshotField::Horizontal,
            "ratio" => SnapshotField::Ratio,
            "first" => SnapshotField::First,
            "second" => SnapshotField::Second,
            "root" => SnapshotField::Root,
            "focused_session_id" => SnapshotField::FocusedSessionId,
            "pinned" => SnapshotField::Pinned,
            "marked" => SnapshotField::Marked,
            "private_title" => SnapshotField::PrivateTitle,
            _ => SnapshotField::Unknown,
        })
    }
}

fn bounded_display_value(value: &str, limit: usize) -> String {
    // Locate the trim range after filtering controls, then copy at most the
    // retained byte ceiling. A final trim makes the operation idempotent when
    // truncation exposes whitespace that was internal in the original value.
    let mut start = None;
    let mut end = 0;
    for (offset, ch) in value.char_indices() {
        if ch.is_control() || is_bidi_display_control(ch) || ch.is_whitespace() {
            continue;
        }
        start.get_or_insert(offset);
        end = offset + ch.len_utf8();
    }

    let Some(start) = start else {
        return String::new();
    };
    let mut bounded = String::with_capacity((end - start).min(limit));
    for ch in value[start..end].chars() {
        if ch.is_control() || is_bidi_display_control(ch) {
            continue;
        }
        if bounded.len() + ch.len_utf8() > limit {
            break;
        }
        bounded.push(ch);
    }
    let trimmed_len = bounded.trim_end().len();
    bounded.truncate(trimmed_len);
    bounded
}

struct DisplayTextSeed<'a> {
    budget: &'a mut DecodeBudget,
    field: &'static str,
    limit: usize,
    empty_default: Option<&'static str>,
}

impl<'de> serde::de::DeserializeSeed<'de> for DisplayTextSeed<'_> {
    type Value = String;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl<'de> serde::de::Visitor<'de> for DisplayTextSeed<'_> {
    type Value = String;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "bounded display text for '{}'", self.field)
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        let mut bounded = bounded_display_value(value, self.limit);
        if bounded.is_empty() {
            if let Some(default) = self.empty_default {
                bounded.push_str(default);
            }
        }
        self.budget.charge_text::<E>(self.field, bounded.len())?;
        if bounded != value {
            self.budget.repaired_fields += 1;
        }
        Ok(bounded)
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
        self.visit_str(&value)
    }
}

enum OptionalTextKind {
    Cwd,
    SessionId,
    CustomName,
    FocusedSessionId,
}

struct OptionalTextSeed<'a> {
    budget: &'a mut DecodeBudget,
    kind: OptionalTextKind,
}

impl<'de> serde::de::DeserializeSeed<'de> for OptionalTextSeed<'_> {
    type Value = Option<String>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_option(self)
    }
}

impl<'de> serde::de::Visitor<'de> for OptionalTextSeed<'_> {
    type Value = Option<String>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("null or bounded session text")
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(None)
    }

    fn visit_some<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(OptionalTextValueVisitor {
            budget: self.budget,
            kind: self.kind,
        })
    }
}

struct OptionalTextValueVisitor<'a> {
    budget: &'a mut DecodeBudget,
    kind: OptionalTextKind,
}

impl OptionalTextValueVisitor<'_> {
    fn decode<E: serde::de::Error>(self, value: &str) -> Result<Option<String>, E> {
        let (field, retained) = match self.kind {
            OptionalTextKind::Cwd => (
                "cwd",
                (value.len() <= MAX_SESSION_CWD_BYTES && !value.as_bytes().contains(&0))
                    .then(|| value.to_owned()),
            ),
            OptionalTextKind::SessionId => (
                "session_id",
                crate::session::is_valid_jsh_session_id(value).then(|| value.to_owned()),
            ),
            OptionalTextKind::CustomName => {
                let bounded = bounded_display_value(value, MAX_SESSION_NAME_BYTES);
                ("custom_name", (!bounded.is_empty()).then_some(bounded))
            }
            OptionalTextKind::FocusedSessionId => (
                "focused_session_id",
                (value.len() <= MAX_SESSION_ID_BYTES).then(|| value.to_owned()),
            ),
        };
        if retained.as_deref() != Some(value) {
            self.budget.repaired_fields += 1;
        }
        if let Some(retained) = retained {
            self.budget.charge_text::<E>(field, retained.len())?;
            Ok(Some(retained))
        } else {
            Ok(None)
        }
    }
}

impl<'de> serde::de::Visitor<'de> for OptionalTextValueVisitor<'_> {
    type Value = Option<String>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("bounded session text")
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        self.decode(value)
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
        self.decode(&value)
    }
}

struct TagsSeed<'a> {
    budget: &'a mut DecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for TagsSeed<'_> {
    type Value = Vec<String>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for TagsSeed<'_> {
    type Value = Vec<String>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "at most {MAX_SESSION_TAGS} session tags")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let budget = self.budget;
        let mut tags = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(MAX_SESSION_TAGS));
        while tags.len() < MAX_SESSION_TAGS {
            let Some(tag) = seq.next_element_seed(DisplayTextSeed {
                budget: &mut *budget,
                field: "tag",
                limit: MAX_SESSION_TAG_BYTES,
                empty_default: None,
            })?
            else {
                return Ok(tags);
            };
            tags.push(tag);
        }
        let mut discarded = false;
        while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
            discarded = true;
        }
        if discarded {
            budget.repaired_fields += 1;
        }
        Ok(tags)
    }
}

struct SessionSeed<'a> {
    budget: &'a mut DecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for SessionSeed<'_> {
    type Value = SessionSnapshot;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for SessionSeed<'_> {
    type Value = SessionSnapshot;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a bounded session snapshot")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;

        let budget = self.budget;
        let mut name = None;
        let mut tags = None;
        let mut cwd: Option<Option<String>> = None;
        let mut session_id: Option<Option<String>> = None;
        let mut custom_name: Option<Option<String>> = None;
        while let Some(key) = map.next_key::<SnapshotField>()? {
            match key {
                SnapshotField::Name => {
                    if name.is_some() {
                        return Err(A::Error::duplicate_field("name"));
                    }
                    name = Some(map.next_value_seed(DisplayTextSeed {
                        budget: &mut *budget,
                        field: "name",
                        limit: MAX_SESSION_NAME_BYTES,
                        empty_default: Some("Session"),
                    })?);
                }
                SnapshotField::Tags => {
                    if tags.is_some() {
                        return Err(A::Error::duplicate_field("tags"));
                    }
                    tags = Some(map.next_value_seed(TagsSeed {
                        budget: &mut *budget,
                    })?);
                }
                SnapshotField::Cwd => {
                    if cwd.is_some() {
                        return Err(A::Error::duplicate_field("cwd"));
                    }
                    cwd = Some(map.next_value_seed(OptionalTextSeed {
                        budget: &mut *budget,
                        kind: OptionalTextKind::Cwd,
                    })?);
                }
                SnapshotField::SessionId => {
                    if session_id.is_some() {
                        return Err(A::Error::duplicate_field("session_id"));
                    }
                    session_id = Some(map.next_value_seed(OptionalTextSeed {
                        budget: &mut *budget,
                        kind: OptionalTextKind::SessionId,
                    })?);
                }
                SnapshotField::CustomName => {
                    if custom_name.is_some() {
                        return Err(A::Error::duplicate_field("custom_name"));
                    }
                    custom_name = Some(map.next_value_seed(OptionalTextSeed {
                        budget: &mut *budget,
                        kind: OptionalTextKind::CustomName,
                    })?);
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(SessionSnapshot {
            name: name.ok_or_else(|| A::Error::missing_field("name"))?,
            tags: tags.ok_or_else(|| A::Error::missing_field("tags"))?,
            cwd: cwd.unwrap_or(None),
            session_id: session_id.unwrap_or(None),
            custom_name: custom_name.unwrap_or(None),
        })
    }
}

struct SessionsSeed<'a> {
    budget: &'a mut DecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for SessionsSeed<'_> {
    type Value = Vec<SessionSnapshot>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for SessionsSeed<'_> {
    type Value = Vec<SessionSnapshot>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "at most {MAX_RESTORED_SESSIONS} sessions")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let budget = self.budget;
        let mut sessions =
            Vec::with_capacity(seq.size_hint().unwrap_or(0).min(MAX_RESTORED_SESSIONS));
        while sessions.len() < MAX_RESTORED_SESSIONS {
            let Some(session) = seq.next_element_seed(SessionSeed {
                budget: &mut *budget,
            })?
            else {
                return Ok(sessions);
            };
            sessions.push(session);
        }
        while seq.next_element_seed(DiscardSessionSeed)?.is_some() {
            budget.extra_sessions = budget.extra_sessions.saturating_add(1);
        }
        Ok(sessions)
    }
}

/// A string that is type-checked but never owned, for validating content that
/// is decoded past a retention limit.
struct DiscardStringSeed;

impl<'de> serde::de::DeserializeSeed<'de> for DiscardStringSeed {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl<'de> serde::de::Visitor<'de> for DiscardStringSeed {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a string")
    }

    fn visit_str<E: serde::de::Error>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(())
    }
}

struct DiscardOptionalTextSeed;

impl<'de> serde::de::DeserializeSeed<'de> for DiscardOptionalTextSeed {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_option(self)
    }
}

impl<'de> serde::de::Visitor<'de> for DiscardOptionalTextSeed {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("null or a string")
    }

    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_some<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(DiscardStringSeed)
    }
}

struct DiscardTagsSeed;

impl<'de> serde::de::DeserializeSeed<'de> for DiscardTagsSeed {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for DiscardTagsSeed {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a list of strings")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        while seq.next_element_seed(DiscardStringSeed)?.is_some() {}
        Ok(())
    }
}

/// Sessions beyond the retained prefix are still schema-validated, just
/// without retaining their text. Derived Serde used to reject a wrong-typed,
/// duplicated, or missing known field at any array position, so truncating
/// the list must not silently broaden the schema.
struct DiscardSessionSeed;

impl<'de> serde::de::DeserializeSeed<'de> for DiscardSessionSeed {
    type Value = ();

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for DiscardSessionSeed {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a session snapshot")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;

        let mut name = false;
        let mut tags = false;
        let mut cwd = false;
        let mut session_id = false;
        let mut custom_name = false;
        while let Some(key) = map.next_key::<SnapshotField>()? {
            match key {
                SnapshotField::Name => {
                    if name {
                        return Err(A::Error::duplicate_field("name"));
                    }
                    name = true;
                    map.next_value_seed(DiscardStringSeed)?;
                }
                SnapshotField::Tags => {
                    if tags {
                        return Err(A::Error::duplicate_field("tags"));
                    }
                    tags = true;
                    map.next_value_seed(DiscardTagsSeed)?;
                }
                SnapshotField::Cwd => {
                    if cwd {
                        return Err(A::Error::duplicate_field("cwd"));
                    }
                    cwd = true;
                    map.next_value_seed(DiscardOptionalTextSeed)?;
                }
                SnapshotField::SessionId => {
                    if session_id {
                        return Err(A::Error::duplicate_field("session_id"));
                    }
                    session_id = true;
                    map.next_value_seed(DiscardOptionalTextSeed)?;
                }
                SnapshotField::CustomName => {
                    if custom_name {
                        return Err(A::Error::duplicate_field("custom_name"));
                    }
                    custom_name = true;
                    map.next_value_seed(DiscardOptionalTextSeed)?;
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        if !name {
            return Err(A::Error::missing_field("name"));
        }
        if !tags {
            return Err(A::Error::missing_field("tags"));
        }
        Ok(())
    }
}

/// The envelope borrows every nested payload as a raw slice of the input, so
/// an unsupported version is rejected before any owned session or layout data
/// exists, and a malformed optional layout never gets cloned once per ancestor
/// while it is being skipped.
struct RawSessionsSnapshot<'de> {
    version: u32,
    sessions: &'de serde_json::value::RawValue,
    active_index: Option<usize>,
    layout: Option<&'de serde_json::value::RawValue>,
    tabs: Option<&'de serde_json::value::RawValue>,
    active_tab: Option<usize>,
}

struct RawSessionsSeed;

impl<'de> serde::de::DeserializeSeed<'de> for RawSessionsSeed {
    type Value = RawSessionsSnapshot<'de>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for RawSessionsSeed {
    type Value = RawSessionsSnapshot<'de>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a versioned sessions snapshot")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;

        let mut version = None;
        let mut sessions = DeferredRawField::default();
        let mut active_index: Option<Option<usize>> = None;
        let mut layout = DeferredRawField::default();
        let mut tabs = DeferredRawField::default();
        let mut active_tab: Option<Option<usize>> = None;
        while let Some(key) = map.next_key::<SnapshotField>()? {
            match key {
                SnapshotField::Version => {
                    if version.is_some() {
                        return Err(A::Error::duplicate_field("version"));
                    }
                    let decoded = map.next_value::<u32>()?;
                    if !(1..=4).contains(&decoded) {
                        return Err(A::Error::custom(format_args!(
                            "unsupported session snapshot version {decoded}"
                        )));
                    }
                    version = Some(decoded);
                }
                SnapshotField::Sessions => sessions.read(&mut map)?,
                SnapshotField::ActiveIndex => {
                    if active_index.is_some() {
                        return Err(A::Error::duplicate_field("active_index"));
                    }
                    active_index = Some(map.next_value::<Option<usize>>()?);
                }
                SnapshotField::Layout => layout.read(&mut map)?,
                SnapshotField::Tabs => tabs.read(&mut map)?,
                SnapshotField::ActiveTab => {
                    if active_tab.is_some() {
                        return Err(A::Error::duplicate_field("active_tab"));
                    }
                    active_tab = Some(map.next_value::<Option<usize>>()?);
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(RawSessionsSnapshot {
            version: version.ok_or_else(|| A::Error::missing_field("version"))?,
            sessions: sessions.required::<A::Error>("sessions")?,
            active_index: active_index.unwrap_or(None),
            layout: layout.optional::<A::Error>("layout")?,
            tabs: tabs.optional::<A::Error>("tabs")?,
            active_tab: active_tab.unwrap_or(None),
        })
    }
}

use jterm_core::bounded_json::DeferredRawField;

struct LayoutSessionIdSeed<'a> {
    budget: &'a mut DecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for LayoutSessionIdSeed<'_> {
    type Value = Option<String>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl<'de> serde::de::Visitor<'de> for LayoutSessionIdSeed<'_> {
    type Value = Option<String>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "a layout session ID of at most {MAX_SESSION_ID_BYTES} bytes"
        )
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        if value.len() > MAX_SESSION_ID_BYTES {
            self.budget.layout_repaired = true;
            return Ok(None);
        }
        self.budget
            .charge_text::<E>("layout session_id", value.len())?;
        Ok(Some(value.to_owned()))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
        self.visit_str(&value)
    }
}

fn decode_layout_session_id(
    raw: &serde_json::value::RawValue,
    budget: &mut DecodeBudget,
) -> Result<Option<String>, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let session_id =
        serde::de::DeserializeSeed::deserialize(LayoutSessionIdSeed { budget }, &mut deserializer)?;
    deserializer.end()?;
    Ok(session_id)
}

#[derive(Clone, Copy)]
enum LayoutKind {
    Pane,
    Split,
}

struct LayoutKindSeed;

impl<'de> serde::de::DeserializeSeed<'de> for LayoutKindSeed {
    type Value = LayoutKind;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl serde::de::Visitor<'_> for LayoutKindSeed {
    type Value = LayoutKind;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("'pane' or 'split'")
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        match value {
            "pane" => Ok(LayoutKind::Pane),
            "split" => Ok(LayoutKind::Split),
            other => Err(E::unknown_variant(other, &["pane", "split"])),
        }
    }
}

/// A layout node whose variant fields are still raw slices of the input. The
/// kind decides which of them are decoded at all: a Pane never spends node or
/// text budgets on Split-only data, and vice versa.
struct RawLayoutNode<'de> {
    kind: LayoutKind,
    session_id: DeferredRawField<'de>,
    horizontal: DeferredRawField<'de>,
    ratio: DeferredRawField<'de>,
    first: DeferredRawField<'de>,
    second: DeferredRawField<'de>,
}

struct RawLayoutNodeSeed;

impl<'de> serde::de::DeserializeSeed<'de> for RawLayoutNodeSeed {
    type Value = RawLayoutNode<'de>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for RawLayoutNodeSeed {
    type Value = RawLayoutNode<'de>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a bounded pane-layout node")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;

        let mut kind = None;
        let mut session_id = DeferredRawField::default();
        let mut horizontal = DeferredRawField::default();
        let mut ratio = DeferredRawField::default();
        let mut first = DeferredRawField::default();
        let mut second = DeferredRawField::default();
        while let Some(key) = map.next_key::<SnapshotField>()? {
            match key {
                SnapshotField::Kind => {
                    if kind.is_some() {
                        return Err(A::Error::duplicate_field("kind"));
                    }
                    kind = Some(map.next_value_seed(LayoutKindSeed)?);
                }
                SnapshotField::SessionId => {
                    session_id.read(&mut map)?;
                }
                SnapshotField::Horizontal => {
                    horizontal.read(&mut map)?;
                }
                SnapshotField::Ratio => {
                    ratio.read(&mut map)?;
                }
                SnapshotField::First => {
                    first.read(&mut map)?;
                }
                SnapshotField::Second => {
                    second.read(&mut map)?;
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(RawLayoutNode {
            kind: kind.ok_or_else(|| A::Error::missing_field("kind"))?,
            session_id,
            horizontal,
            ratio,
            first,
            second,
        })
    }
}

/// Depth and node budgets prune an oversized branch instead of failing its
/// tab. The raw slice was already syntax-checked when it was captured, so a
/// pruned node needs no parser at all.
fn decode_layout_node(
    raw: &serde_json::value::RawValue,
    budget: &mut DecodeBudget,
    depth: usize,
) -> Result<Option<LayoutNodeSnapshot>, serde_json::Error> {
    if depth > MAX_RESTORED_LAYOUT_DEPTH || budget.remaining_layout_nodes == 0 {
        budget.layout_repaired = true;
        return Ok(None);
    }
    budget.remaining_layout_nodes -= 1;

    // Finish and drop this parser before following any child. serde_json keeps
    // a scratch buffer while skipping RawValue contents; recursing from inside
    // the visitor would retain one near-file-sized buffer per ancestor.
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let staged = serde::de::DeserializeSeed::deserialize(RawLayoutNodeSeed, &mut deserializer)?;
    deserializer.end()?;
    drop(deserializer);

    match staged.kind {
        LayoutKind::Pane => {
            let raw = staged
                .session_id
                .required::<serde_json::Error>("session_id")?;
            let session_id = decode_layout_session_id(raw, budget)?;
            Ok(session_id.map(|session_id| LayoutNodeSnapshot::Pane { session_id }))
        }
        LayoutKind::Split => {
            let horizontal = serde_json::from_str::<bool>(
                staged
                    .horizontal
                    .required::<serde_json::Error>("horizontal")?
                    .get(),
            )?;
            let ratio = staged
                .ratio
                .optional::<serde_json::Error>("ratio")?
                .map(|raw| serde_json::from_str::<f32>(raw.get()))
                .transpose()?
                .unwrap_or_else(default_split_ratio);
            let first = decode_layout_node(
                staged.first.required::<serde_json::Error>("first")?,
                budget,
                depth + 1,
            )?;
            let second = decode_layout_node(
                staged.second.required::<serde_json::Error>("second")?,
                budget,
                depth + 1,
            )?;
            match (first, second) {
                (Some(first), Some(second)) => Ok(Some(LayoutNodeSnapshot::Split {
                    horizontal,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                })),
                (Some(remaining), None) | (None, Some(remaining)) => {
                    budget.layout_repaired = true;
                    Ok(Some(remaining))
                }
                (None, None) => {
                    budget.layout_repaired = true;
                    Ok(None)
                }
            }
        }
    }
}

/// One tab layout. Only `root` can grow without bound, so it stays a raw
/// slice until this tab's own parser has been dropped; the small scalar
/// fields are validated inline.
struct RawTabLayout<'de> {
    root: DeferredRawField<'de>,
    focused_session_id: Option<String>,
    pinned: bool,
    marked: bool,
    private_title: bool,
}

struct RawTabLayoutSeed<'a> {
    budget: &'a mut DecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for RawTabLayoutSeed<'_> {
    type Value = RawTabLayout<'de>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> serde::de::Visitor<'de> for RawTabLayoutSeed<'_> {
    type Value = RawTabLayout<'de>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a bounded tab layout")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;

        let mut root = DeferredRawField::default();
        let mut focused_session_id: Option<Option<String>> = None;
        let mut pinned = None;
        let mut marked = None;
        let mut private_title = None;
        while let Some(key) = map.next_key::<SnapshotField>()? {
            match key {
                SnapshotField::Root => {
                    root.read(&mut map)?;
                }
                SnapshotField::FocusedSessionId => {
                    if focused_session_id.is_some() {
                        return Err(A::Error::duplicate_field("focused_session_id"));
                    }
                    focused_session_id = Some(map.next_value_seed(OptionalTextSeed {
                        budget: self.budget,
                        kind: OptionalTextKind::FocusedSessionId,
                    })?);
                }
                SnapshotField::Pinned => {
                    if pinned.is_some() {
                        return Err(A::Error::duplicate_field("pinned"));
                    }
                    pinned = Some(map.next_value::<bool>()?);
                }
                SnapshotField::Marked => {
                    if marked.is_some() {
                        return Err(A::Error::duplicate_field("marked"));
                    }
                    marked = Some(map.next_value::<bool>()?);
                }
                SnapshotField::PrivateTitle => {
                    if private_title.is_some() {
                        return Err(A::Error::duplicate_field("private_title"));
                    }
                    private_title = Some(map.next_value::<bool>()?);
                }
                _ => {
                    map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(RawTabLayout {
            root,
            focused_session_id: focused_session_id.unwrap_or(None),
            pinned: pinned.unwrap_or(false),
            marked: marked.unwrap_or(false),
            private_title: private_title.unwrap_or(false),
        })
    }
}

fn decode_tab(
    raw: &serde_json::value::RawValue,
    budget: &mut DecodeBudget,
) -> Result<LayoutSnapshot, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let staged = serde::de::DeserializeSeed::deserialize(
        RawTabLayoutSeed {
            budget: &mut *budget,
        },
        &mut deserializer,
    )?;
    deserializer.end()?;
    drop(deserializer);
    let root = decode_layout_node(
        staged.root.required::<serde_json::Error>("root")?,
        budget,
        0,
    )?
    .ok_or_else(|| {
        <serde_json::Error as serde::de::Error>::custom("layout root was removed by restore limits")
    })?;
    Ok(LayoutSnapshot {
        root,
        focused_session_id: staged.focused_session_id,
        pinned: staged.pinned,
        marked: staged.marked,
        private_title: staged.private_title,
    })
}

struct RawTabs<'de> {
    tabs: Vec<&'de serde_json::value::RawValue>,
    truncated: bool,
}

struct RawTabsSeed;

impl<'de> serde::de::DeserializeSeed<'de> for RawTabsSeed {
    type Value = RawTabs<'de>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

impl<'de> serde::de::Visitor<'de> for RawTabsSeed {
    type Value = RawTabs<'de>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "at most {MAX_RESTORED_TABS} tab layouts")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut tabs = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(MAX_RESTORED_TABS));
        let mut input_count = 0;
        while input_count < MAX_RESTORED_TABS {
            let Some(raw) = seq.next_element::<&'de serde_json::value::RawValue>()? else {
                return Ok(RawTabs {
                    tabs,
                    truncated: false,
                });
            };
            input_count += 1;
            tabs.push(raw);
        }
        let mut truncated = false;
        while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
            truncated = true;
        }
        Ok(RawTabs { tabs, truncated })
    }
}

/// Tabs that survived their own decode, plus where each one sat in the input,
/// so `active_tab` can be remapped onto the tab it originally named.
struct DecodedTabs {
    tabs: Vec<LayoutSnapshot>,
    retained_input_indices: Vec<usize>,
}

impl DecodedTabs {
    fn empty() -> Self {
        Self {
            tabs: Vec::new(),
            retained_input_indices: Vec::new(),
        }
    }
}

/// Each tab decodes transactionally: a malformed or oversized tab rolls its
/// budget charges back and is dropped, but never takes a valid neighbour down
/// with it.
fn decode_tabs(
    raw: &serde_json::value::RawValue,
    budget: &mut DecodeBudget,
) -> Result<DecodedTabs, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let staged = serde::de::DeserializeSeed::deserialize(RawTabsSeed, &mut deserializer)?;
    deserializer.end()?;
    drop(deserializer);
    if staged.truncated {
        budget.layout_repaired = true;
    }
    let mut tabs = Vec::with_capacity(staged.tabs.len());
    let mut retained_input_indices = Vec::with_capacity(staged.tabs.len());
    for (input_index, raw_tab) in staged.tabs.into_iter().enumerate() {
        let before = *budget;
        match decode_tab(raw_tab, budget) {
            Ok(tab) => {
                tabs.push(tab);
                retained_input_indices.push(input_index);
            }
            Err(_) => {
                *budget = before;
                budget.layout_repaired = true;
            }
        }
    }
    Ok(DecodedTabs {
        tabs,
        retained_input_indices,
    })
}

/// `active_tab` names an input position. After transactional discards the
/// retained tabs have shifted, so follow the identity: the same input tab if
/// it survived, otherwise the first surviving tab after it.
fn remap_active_tab(active: Option<usize>, tabs: &DecodedTabs) -> (Option<usize>, bool) {
    let Some(active) = active else {
        return (None, false);
    };
    if tabs.tabs.is_empty() {
        return (None, true);
    }
    let retained = tabs
        .retained_input_indices
        .iter()
        .position(|index| *index == active);
    if let Some(retained) = retained {
        return (Some(retained), false);
    }
    let fallback = tabs
        .retained_input_indices
        .iter()
        .position(|index| *index > active)
        .or_else(|| tabs.tabs.len().checked_sub(1));
    (fallback, true)
}

fn decode_sessions(
    raw: &serde_json::value::RawValue,
    budget: &mut DecodeBudget,
) -> Result<Vec<SessionSnapshot>, serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(raw.get());
    let sessions =
        serde::de::DeserializeSeed::deserialize(SessionsSeed { budget }, &mut deserializer)?;
    deserializer.end()?;
    drop(deserializer);
    Ok(sessions)
}

fn raw_is_null(raw: &serde_json::value::RawValue) -> bool {
    raw.get().trim() == "null"
}

fn decode_bounded_snapshot_with_text_budget(
    content: &str,
    text_budget: usize,
) -> Result<(SessionsSnapshot, Vec<String>), serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_str(content);
    let raw = serde::de::DeserializeSeed::deserialize(RawSessionsSeed, &mut deserializer)?;
    deserializer.end()?;
    drop(deserializer);

    let mut budget = DecodeBudget::new(text_budget);
    let sessions = decode_sessions(raw.sessions, &mut budget)?;
    let decoded_tabs = match raw.tabs {
        Some(raw_tabs) => {
            let before = budget;
            match decode_tabs(raw_tabs, &mut budget) {
                Ok(tabs) => tabs,
                Err(_) => {
                    budget = before;
                    budget.layout_repaired = true;
                    DecodedTabs::empty()
                }
            }
        }
        None => DecodedTabs::empty(),
    };
    let (active_tab, active_tab_repaired) = remap_active_tab(raw.active_tab, &decoded_tabs);
    budget.active_tab_repaired = active_tab_repaired;

    // The v3-and-earlier global tree only matters when no per-tab layout
    // survived; `sanitize` then migrates it into the first tab.
    let mut layout = None;
    if decoded_tabs.tabs.is_empty() {
        if let Some(raw_layout) = raw.layout.filter(|raw| !raw_is_null(raw)) {
            let before = budget;
            match decode_tab(raw_layout, &mut budget) {
                Ok(decoded) => layout = Some(decoded),
                Err(_) => {
                    budget = before;
                    budget.layout_repaired = true;
                }
            }
        }
    }

    let mut warnings = Vec::new();
    if budget.extra_sessions > 0 {
        warnings.push(format!(
            "restored only the first {MAX_RESTORED_SESSIONS} of {} sessions",
            sessions.len() + budget.extra_sessions
        ));
    }
    if budget.repaired_fields > 0 {
        warnings.push(format!(
            "repaired {} oversized or invalid session fields",
            budget.repaired_fields
        ));
    }
    if budget.layout_repaired {
        warnings.push("repaired an invalid or oversized pane layout".to_string());
    }
    if budget.active_tab_repaired {
        warnings.push("active tab index was outside the restored list".to_string());
    }

    Ok((
        SessionsSnapshot {
            version: raw.version,
            sessions,
            active_index: raw.active_index,
            layout,
            tabs: decoded_tabs.tabs,
            active_tab,
        },
        warnings,
    ))
}

fn decode_bounded_snapshot(
    content: &str,
) -> Result<(SessionsSnapshot, Vec<String>), serde_json::Error> {
    decode_bounded_snapshot_with_text_budget(content, MAX_RESTORED_TEXT_BYTES)
}

impl SessionsSnapshot {
    /// 从会话快照列表创建
    pub fn from_snapshots(
        sessions: Vec<SessionSnapshot>,
        active_index: Option<usize>,
        tabs: Vec<LayoutSnapshot>,
        active_tab: Option<usize>,
    ) -> Self {
        // `layout` 保留给旧版本读取：给它当前 tab 的布局，旧版本至少能开出
        // 用户最后看到的那组窗格，而不是一个空布局。
        let layout = active_tab
            .and_then(|idx| tabs.get(idx))
            .or_else(|| tabs.first())
            .cloned();
        SessionsSnapshot {
            version: 4,
            sessions,
            active_index,
            layout,
            tabs,
            active_tab,
        }
    }

    /// 保存到文件（原子写入 + fsync 持久化）
    pub fn save(&self, path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
        // Apply the same resource contract on write and read. Runtime metadata
        // can originate in text fields, so writing it verbatim could otherwise
        // create a snapshot that this application rejects on its next launch.
        let mut bounded = self.clone();
        bounded.version = 4;
        let warnings = bounded.sanitize();
        for warning in warnings {
            eprintln!("[SessionPersistence] WARNING while saving: {warning}");
        }
        let json = serde_json::to_vec_pretty(&bounded)?;
        if json.len() as u64 > MAX_SESSION_SNAPSHOT_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                format!(
                    "bounded session snapshot is {} bytes; limit is {}",
                    json.len(),
                    MAX_SESSION_SNAPSHOT_BYTES
                ),
            )
            .into());
        }
        crate::persistence_file::write_atomic(path, &json)?;
        eprintln!("[SessionPersistence] Sessions saved to {}", path.display());
        Ok(())
    }

    /// 从文件加载，并报告为了保证启动资源有界而执行的修复。
    pub fn load_with_warnings(
        path: &std::path::Path,
    ) -> Result<(Self, Vec<String>), Box<dyn std::error::Error>> {
        // The local boundary deliberately fronts the pinned core revision: it
        // opens with O_NOFOLLOW/O_NONBLOCK, validates owner/link count through
        // the descriptor, and keeps the total read under the hard limit.
        let content = match crate::persistence_file::read_bounded(path, MAX_SESSION_SNAPSHOT_BYTES)
        {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok((
                    SessionsSnapshot {
                        version: 4,
                        sessions: vec![],
                        active_index: None,
                        layout: None,
                        tabs: Vec::new(),
                        active_tab: None,
                    },
                    Vec::new(),
                ));
            }
            Err(error) => return Err(error.into()),
        };
        let (mut snapshot, mut warnings) = decode_bounded_snapshot(&content)?;
        for warning in snapshot.sanitize() {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
        eprintln!(
            "[SessionPersistence] Sessions loaded from {}",
            path.display()
        );
        Ok((snapshot, warnings))
    }

    /// Compatibility wrapper for callers that do not need sanitization notes.
    #[allow(dead_code)]
    pub fn load(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        Self::load_with_warnings(path).map(|(snapshot, _warnings)| snapshot)
    }

    fn sanitize(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.sessions.len() > MAX_RESTORED_SESSIONS {
            warnings.push(format!(
                "restored only the first {MAX_RESTORED_SESSIONS} of {} sessions",
                self.sessions.len()
            ));
            self.sessions.truncate(MAX_RESTORED_SESSIONS);
        }

        let mut repaired_fields = 0usize;
        for session in &mut self.sessions {
            repaired_fields += usize::from(sanitize_display_text(
                &mut session.name,
                MAX_SESSION_NAME_BYTES,
            ));
            if session.name.is_empty() {
                session.name = "Session".to_string();
                repaired_fields += 1;
            }
            if session.tags.len() > MAX_SESSION_TAGS {
                session.tags.truncate(MAX_SESSION_TAGS);
                repaired_fields += 1;
            }
            for tag in &mut session.tags {
                repaired_fields += usize::from(sanitize_display_text(tag, MAX_SESSION_TAG_BYTES));
            }
            if session
                .cwd
                .as_ref()
                .is_some_and(|cwd| cwd.len() > MAX_SESSION_CWD_BYTES || cwd.as_bytes().contains(&0))
            {
                session.cwd = None;
                repaired_fields += 1;
            }
            if let Some(name) = session.custom_name.as_mut() {
                repaired_fields += usize::from(sanitize_display_text(name, MAX_SESSION_NAME_BYTES));
                if name.is_empty() {
                    session.custom_name = None;
                    repaired_fields += 1;
                }
            }
            if session
                .session_id
                .as_deref()
                .is_some_and(|id| !crate::session::is_valid_jsh_session_id(id))
            {
                session.session_id = None;
                repaired_fields += 1;
            }
        }
        if repaired_fields > 0 {
            warnings.push(format!(
                "repaired {repaired_fields} oversized or invalid session fields"
            ));
        }

        if self
            .active_index
            .is_some_and(|index| index >= self.sessions.len())
        {
            self.active_index = self.sessions.len().checked_sub(1);
            warnings.push("active session index was outside the restored list".to_string());
        }

        let allowed_session_ids = self
            .sessions
            .iter()
            .filter_map(|session| session.session_id.as_ref())
            .cloned()
            .collect::<HashSet<_>>();
        // 一个会话至多出现在一个窗格里，而这个约束跨 tab 成立——否则两个
        // tab 会争夺同一个 PTY。`used_session_ids` 因此在所有 tab 间共享。
        let mut used_session_ids = HashSet::new();
        let mut node_count = 0usize;
        let mut layout_repaired = false;

        let raw_tabs = std::mem::take(&mut self.tabs);
        let migrating = raw_tabs.is_empty();
        let raw_tabs = if migrating {
            self.layout.take().into_iter().collect()
        } else {
            raw_tabs
        };

        for tab in raw_tabs {
            let root = sanitize_layout_node(
                tab.root,
                &allowed_session_ids,
                &mut used_session_ids,
                &mut node_count,
                0,
                &mut layout_repaired,
            );
            match root {
                Some(root) => {
                    let focused_session_id = tab.focused_session_id.filter(|session_id| {
                        let keep = used_session_ids.contains(session_id);
                        layout_repaired |= !keep;
                        keep
                    });
                    self.tabs.push(LayoutSnapshot {
                        root,
                        focused_session_id,
                        pinned: tab.pinned,
                        marked: tab.marked,
                        private_title: tab.private_title,
                    });
                }
                // 空 tab 不是可渲染的状态，整个丢掉；它的会话会在启动时
                // 作为孤儿各自获得一个新 tab。
                None => layout_repaired = true,
            }
        }

        if self
            .active_tab
            .is_some_and(|index| index >= self.tabs.len())
        {
            self.active_tab = self.tabs.len().checked_sub(1);
            layout_repaired = true;
        }
        self.layout = self
            .active_tab
            .and_then(|idx| self.tabs.get(idx))
            .or_else(|| self.tabs.first())
            .cloned();

        if layout_repaired {
            warnings.push("repaired an invalid or oversized pane layout".to_string());
        }
        warnings
    }
}

fn sanitize_layout_node(
    node: LayoutNodeSnapshot,
    allowed_session_ids: &HashSet<String>,
    used_session_ids: &mut HashSet<String>,
    node_count: &mut usize,
    depth: usize,
    repaired: &mut bool,
) -> Option<LayoutNodeSnapshot> {
    if depth > MAX_RESTORED_LAYOUT_DEPTH || *node_count >= MAX_RESTORED_LAYOUT_NODES {
        *repaired = true;
        return None;
    }
    *node_count += 1;

    match node {
        LayoutNodeSnapshot::Pane { session_id } => {
            if allowed_session_ids.contains(&session_id)
                && used_session_ids.insert(session_id.clone())
            {
                Some(LayoutNodeSnapshot::Pane { session_id })
            } else {
                *repaired = true;
                None
            }
        }
        LayoutNodeSnapshot::Split {
            horizontal,
            ratio,
            first,
            second,
        } => {
            let first = sanitize_layout_node(
                *first,
                allowed_session_ids,
                used_session_ids,
                node_count,
                depth + 1,
                repaired,
            );
            let second = sanitize_layout_node(
                *second,
                allowed_session_ids,
                used_session_ids,
                node_count,
                depth + 1,
                repaired,
            );
            match (first, second) {
                (Some(first), Some(second)) => {
                    let normalized_ratio = if ratio.is_finite() {
                        ratio.clamp(0.1, 0.9)
                    } else {
                        0.5
                    };
                    *repaired |= normalized_ratio != ratio;
                    Some(LayoutNodeSnapshot::Split {
                        horizontal,
                        ratio: normalized_ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    })
                }
                (Some(remaining), None) | (None, Some(remaining)) => {
                    *repaired = true;
                    Some(remaining)
                }
                (None, None) => {
                    *repaired = true;
                    None
                }
            }
        }
    }
}

fn is_bidi_display_control(ch: char) -> bool {
    matches!(
        ch,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

fn sanitize_display_text(value: &mut String, max_bytes: usize) -> bool {
    let bounded = bounded_display_value(value, max_bytes);
    if *value == bounded {
        false
    } else {
        *value = bounded;
        true
    }
}

pub fn bounded_session_name(value: &str) -> String {
    let mut value = value.to_string();
    sanitize_display_text(&mut value, MAX_SESSION_NAME_BYTES);
    value
}

fn validate_instance_lock_file(file: &std::fs::File) -> std::io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "instance lock is not a regular file",
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.nlink() != 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "instance lock must have exactly one hard link",
            ));
        }
        // SAFETY: geteuid has no preconditions and only reads process state.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "instance lock is not owned by the current user",
            ));
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
    }

    Ok(())
}

/// Owns the primary-instance lock and publishes its descriptor so a freshly
/// forked PTY child can close its inherited copy before doing any other work.
/// `FD_CLOEXEC` alone is insufficient because `flock` remains held between
/// `fork` and `execve`, and can survive the parent if that child stalls.
pub struct InstanceLock {
    file: std::fs::File,
}

impl InstanceLock {
    fn register(file: std::fs::File) -> std::io::Result<Self> {
        use std::os::unix::io::AsRawFd;

        let fd = file.as_raw_fd();
        INSTANCE_LOCK_FD
            .compare_exchange(NO_INSTANCE_LOCK_FD, fd, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "an instance lock descriptor is already registered",
                )
            })?;
        Ok(Self { file })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;

        let fd = self.file.as_raw_fd();
        let _ = INSTANCE_LOCK_FD.compare_exchange(
            fd,
            NO_INSTANCE_LOCK_FD,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

/// Capture before `fork`; the child must only use the returned integer with
/// async-signal-safe `close(2)`.
pub(crate) fn inherited_instance_lock_fd() -> libc::c_int {
    INSTANCE_LOCK_FD.load(Ordering::Acquire)
}

fn try_acquire_instance_lock_at(
    lock_path: &std::path::Path,
) -> std::io::Result<Option<std::fs::File>> {
    crate::persistence_file::ensure_parent(lock_path)?;

    // Do not truncate before flock: a losing second instance must leave the
    // lock owner's diagnostic PID intact.
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let mut file = options.open(lock_path)?;
    validate_instance_lock_file(&file)?;

    use std::os::unix::io::AsRawFd;
    let fd = file.as_raw_fd();
    // Retain a small bounded retry for transient duplicated descriptors. PTY
    // children explicitly close the registered descriptor immediately after
    // fork, so application-spawned children cannot extend the lock lifetime.
    let retry_deadline = std::time::Instant::now() + INSTANCE_LOCK_RETRY_WINDOW;
    loop {
        // LOCK_EX | LOCK_NB: 非阻塞排他锁
        // SAFETY: flock 对有效的文件描述符是安全的。fd 来自有效的 File 对象，
        // 标志是合法的 flock 常量。File 对象的生命周期确保 fd 在调用期间有效。
        let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if ret == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(error);
        }
        let now = std::time::Instant::now();
        if now >= retry_deadline {
            return Ok(None);
        }
        std::thread::sleep(
            INSTANCE_LOCK_RETRY_INTERVAL.min(retry_deadline.saturating_duration_since(now)),
        );
    }

    // Re-check after acquiring the lock and immediately before mutation. This
    // rejects a regular file that gained a second directory entry between open
    // and flock instead of truncating another path's contents through a hard
    // link. O_NOFOLLOW above handles symbolic links at open time.
    validate_instance_lock_file(&file)?;

    // Only the lock owner may replace the diagnostic PID.
    use std::io::{Seek, Write};
    file.set_len(0)?;
    file.rewind()?;
    write!(file, "{}", std::process::id())?;
    file.sync_all()?;
    Ok(Some(file))
}

/// 尝试获取实例锁文件。成功返回持锁守卫，失败表示已有实例在运行。
pub fn try_acquire_instance_lock() -> Option<InstanceLock> {
    let lock_path = dirs::config_dir()?.join("ember").join("instance.lock");
    match try_acquire_instance_lock_at(&lock_path) {
        Ok(Some(file)) => match InstanceLock::register(file) {
            Ok(lock) => Some(lock),
            Err(error) => {
                eprintln!(
                    "[SessionPersistence] Failed to register instance lock {}: {}",
                    lock_path.display(),
                    error
                );
                None
            }
        },
        Ok(None) => None,
        Err(error) => {
            eprintln!(
                "[SessionPersistence] Failed to acquire instance lock {}: {}",
                lock_path.display(),
                error
            );
            None
        }
    }
}

/// 确保会话历史目录存在
pub fn ensure_session_history_dir(
    path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    crate::persistence_file::ensure_parent(path).map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_LOCK_TEST: AtomicU64 = AtomicU64::new(0);

    fn write_private(path: &std::path::Path, contents: impl AsRef<[u8]>) {
        std::fs::write(path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    struct TestDir(std::path::PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let id = NEXT_LOCK_TEST.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "ember-instance-lock-test-{}-{label}-{id}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    struct ForkChild(libc::pid_t);

    #[cfg(unix)]
    impl Drop for ForkChild {
        fn drop(&mut self) {
            // SAFETY: the stored PID was returned by fork in this test. SIGKILL
            // guarantees a child blocked in pause exits, and waitpid reaps it.
            unsafe {
                let _ = libc::kill(self.0, libc::SIGKILL);
                let mut status = 0;
                while libc::waitpid(self.0, &mut status, 0) < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                }
            }
        }
    }

    #[test]
    fn test_snapshot_conversion() {
        let snapshots = vec![
            SessionSnapshot {
                name: "Session 1".to_string(),
                tags: vec!["dev".to_string()],
                cwd: Some("/home/user".to_string()),
                session_id: Some("123-456".to_string()),
                custom_name: None,
            },
            SessionSnapshot {
                name: "Session 2".to_string(),
                tags: vec!["test".to_string()],
                cwd: Some("/tmp".to_string()),
                session_id: None,
                custom_name: None,
            },
        ];

        let layout = LayoutSnapshot {
            root: LayoutNodeSnapshot::Split {
                horizontal: false,
                ratio: 0.6,
                first: Box::new(LayoutNodeSnapshot::Pane {
                    session_id: "123-456".to_string(),
                }),
                second: Box::new(LayoutNodeSnapshot::Pane {
                    session_id: "second-session".to_string(),
                }),
            },
            focused_session_id: Some("second-session".to_string()),
            pinned: false,
            marked: false,
            private_title: false,
        };
        let snapshot =
            SessionsSnapshot::from_snapshots(snapshots, Some(1), vec![layout.clone()], Some(0));
        assert_eq!(snapshot.sessions.len(), 2);
        assert_eq!(snapshot.sessions[0].cwd, Some("/home/user".to_string()));
        assert_eq!(snapshot.sessions[1].cwd, Some("/tmp".to_string()));
        assert_eq!(snapshot.active_index, Some(1));
        assert_eq!(snapshot.version, 4);
        assert_eq!(snapshot.tabs, vec![layout.clone()]);
        // 旧字段仍然写出当前 tab 的布局，供旧版本读取。
        assert_eq!(snapshot.layout, Some(layout.clone()));

        let encoded = serde_json::to_string(&snapshot).unwrap();
        let decoded: SessionsSnapshot = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.tabs, vec![layout.clone()]);
        assert_eq!(decoded.active_tab, Some(0));
        assert_eq!(decoded.layout, Some(layout));
    }

    #[test]
    fn test_backward_compat_deserialization() {
        let json =
            r#"{"version":1,"sessions":[{"name":"Session 1","tags":[],"cwd":"/home/user"}]}"#;
        let snapshot: SessionsSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snapshot.sessions[0].session_id, None);
        assert_eq!(snapshot.active_index, None);
        assert_eq!(snapshot.layout, None);
    }

    #[test]
    fn malformed_layout_does_not_prevent_session_restore() {
        let json = r#"{
            "version": 3,
            "sessions": [{"name": "Session 1", "tags": []}],
            "active_index": 0,
            "layout": {"root": {"kind": "unknown"}}
        }"#;
        let snapshot: SessionsSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snapshot.sessions.len(), 1);
        assert_eq!(snapshot.active_index, Some(0));
        assert_eq!(snapshot.layout, None);
    }

    /// v3 快照只有一棵全局布局树。它描述的是用户最后看到的那组窗格,所以
    /// 迁移时应该原样变成第一个 tab,而不是散成一堆单窗格 tab。
    #[test]
    fn a_v3_snapshot_migrates_its_single_layout_into_the_first_tab() {
        let json = r#"{
            "version": 3,
            "sessions": [
                {"name": "a", "tags": [], "session_id": "session-a"},
                {"name": "b", "tags": [], "session_id": "session-b"}
            ],
            "active_index": 1,
            "layout": {
                "root": {
                    "kind": "split",
                    "horizontal": true,
                    "ratio": 0.5,
                    "first": {"kind": "pane", "session_id": "session-a"},
                    "second": {"kind": "pane", "session_id": "session-b"}
                },
                "focused_session_id": "session-b"
            }
        }"#;
        let mut snapshot: SessionsSnapshot = serde_json::from_str(json).unwrap();
        assert!(snapshot.tabs.is_empty());

        let warnings = snapshot.sanitize();

        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(snapshot.tabs.len(), 1);
        assert_eq!(
            snapshot.tabs[0].focused_session_id.as_deref(),
            Some("session-b")
        );
        assert!(matches!(
            snapshot.tabs[0].root,
            LayoutNodeSnapshot::Split { .. }
        ));
    }

    /// 同一个会话不能同时出现在两个 tab 的窗格里,否则两个 tab 会争夺同一个
    /// PTY——去重必须跨 tab 生效,而不只是在单棵树内。
    #[test]
    fn a_session_cannot_appear_in_two_tabs() {
        let pane = |id: &str| LayoutSnapshot {
            root: LayoutNodeSnapshot::Pane {
                session_id: id.to_string(),
            },
            focused_session_id: Some(id.to_string()),
            pinned: false,
            marked: false,
            private_title: false,
        };
        let mut snapshot = SessionsSnapshot::from_snapshots(
            vec![SessionSnapshot {
                name: "a".to_string(),
                tags: vec![],
                cwd: None,
                session_id: Some("session-a".to_string()),
                custom_name: None,
            }],
            Some(0),
            vec![pane("session-a"), pane("session-a")],
            Some(0),
        );

        let warnings = snapshot.sanitize();

        assert_eq!(snapshot.tabs.len(), 1);
        assert!(!warnings.is_empty());
    }

    #[test]
    fn an_out_of_range_active_tab_is_clamped() {
        let mut snapshot = SessionsSnapshot::from_snapshots(
            vec![SessionSnapshot {
                name: "a".to_string(),
                tags: vec![],
                cwd: None,
                session_id: Some("session-a".to_string()),
                custom_name: None,
            }],
            Some(0),
            vec![LayoutSnapshot {
                root: LayoutNodeSnapshot::Pane {
                    session_id: "session-a".to_string(),
                },
                focused_session_id: None,
                pinned: false,
                marked: false,
                private_title: false,
            }],
            Some(9),
        );

        snapshot.sanitize();

        assert_eq!(snapshot.active_tab, Some(0));
    }

    #[test]
    fn restored_sessions_and_fields_are_bounded_before_spawn() {
        let session = SessionSnapshot {
            name: "雪".repeat(200),
            tags: (0..(MAX_SESSION_TAGS + 10))
                .map(|_| "x".repeat(MAX_SESSION_TAG_BYTES + 10))
                .collect(),
            cwd: Some("/".repeat(MAX_SESSION_CWD_BYTES + 10)),
            session_id: Some("../invalid".to_string()),
            custom_name: Some("n".repeat(MAX_SESSION_NAME_BYTES + 10)),
        };
        let mut snapshot = SessionsSnapshot::from_snapshots(
            vec![session; MAX_RESTORED_SESSIONS + 10],
            Some(usize::MAX),
            Vec::new(),
            None,
        );

        let warnings = snapshot.sanitize();

        assert_eq!(snapshot.sessions.len(), MAX_RESTORED_SESSIONS);
        assert_eq!(
            snapshot.active_index,
            Some(MAX_RESTORED_SESSIONS.saturating_sub(1))
        );
        assert!(warnings.len() >= 2);
        for session in snapshot.sessions {
            assert!(session.name.len() <= MAX_SESSION_NAME_BYTES);
            assert!(session.name.is_char_boundary(session.name.len()));
            assert_eq!(session.tags.len(), MAX_SESSION_TAGS);
            assert!(session
                .tags
                .iter()
                .all(|tag| tag.len() <= MAX_SESSION_TAG_BYTES));
            assert!(session.cwd.is_none());
            assert!(session.session_id.is_none());
            assert!(session
                .custom_name
                .as_ref()
                .is_some_and(|name| name.len() <= MAX_SESSION_NAME_BYTES));
        }
    }

    #[test]
    fn save_produces_a_snapshot_accepted_by_the_bounded_loader() {
        let root = TestDir::new("bounded-save-round-trip");
        let path = root.0.join("sessions.json");
        let huge_name = "x".repeat(MAX_SESSION_SNAPSHOT_BYTES as usize + 1024);
        let mut sessions = vec![SessionSnapshot {
            name: huge_name.clone(),
            tags: vec![],
            cwd: Some("/tmp".to_string()),
            session_id: Some("session-0".to_string()),
            custom_name: Some(huge_name),
        }];
        sessions.extend((1..MAX_RESTORED_SESSIONS + 8).map(|index| SessionSnapshot {
            name: format!("Session {index}"),
            tags: vec![],
            cwd: Some("/tmp".to_string()),
            session_id: Some(format!("session-{index}")),
            custom_name: None,
        }));
        let snapshot = SessionsSnapshot::from_snapshots(
            sessions,
            Some(usize::MAX),
            vec![LayoutSnapshot {
                root: LayoutNodeSnapshot::Split {
                    horizontal: false,
                    ratio: f32::NAN,
                    first: Box::new(LayoutNodeSnapshot::Pane {
                        session_id: "session-0".to_string(),
                    }),
                    second: Box::new(LayoutNodeSnapshot::Pane {
                        session_id: "session-0".to_string(),
                    }),
                },
                focused_session_id: Some("session-0".to_string()),
                pinned: false,
                marked: false,
                private_title: false,
            }],
            Some(0),
        );

        snapshot.save(&path).unwrap();

        assert!(std::fs::metadata(&path).unwrap().len() <= MAX_SESSION_SNAPSHOT_BYTES);
        let (loaded, warnings) = SessionsSnapshot::load_with_warnings(&path).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(loaded.sessions.len(), MAX_RESTORED_SESSIONS);
        assert_eq!(loaded.sessions[0].name.len(), MAX_SESSION_NAME_BYTES);
        assert_eq!(
            loaded.sessions[0].custom_name.as_ref().unwrap().len(),
            MAX_SESSION_NAME_BYTES
        );
        assert_eq!(
            loaded.active_index,
            Some(MAX_RESTORED_SESSIONS.saturating_sub(1))
        );
        assert!(matches!(
            loaded.layout.unwrap().root,
            LayoutNodeSnapshot::Pane { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_save_preserves_a_shared_parent_and_refuses_a_linked_parent() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = TestDir::new("configured-parent");
        let shared = root.0.join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
        let snapshot = SessionsSnapshot::from_snapshots(Vec::new(), None, Vec::new(), None);

        snapshot.save(&shared.join("sessions.json")).unwrap();
        assert_eq!(
            std::fs::metadata(&shared).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let victim = root.0.join("victim");
        let linked_parent = root.0.join("linked-parent");
        std::fs::create_dir(&victim).unwrap();
        symlink(&victim, &linked_parent).unwrap();
        assert!(ensure_session_history_dir(&linked_parent.join("sessions.json")).is_err());
        assert!(snapshot.save(&linked_parent.join("sessions.json")).is_err());
        assert!(!victim.join("sessions.json").exists());
    }

    #[test]
    fn load_with_warnings_sanitizes_the_real_file_path() {
        let root = TestDir::new("bounded-load-integration");
        let path = root.0.join("sessions.json");
        let session = SessionSnapshot {
            name: "雪".repeat(200),
            tags: vec!["x".repeat(MAX_SESSION_TAG_BYTES + 1); MAX_SESSION_TAGS + 1],
            cwd: Some("/".repeat(MAX_SESSION_CWD_BYTES + 1)),
            session_id: Some("../invalid".to_string()),
            custom_name: Some("n".repeat(MAX_SESSION_NAME_BYTES + 1)),
        };
        let snapshot = SessionsSnapshot::from_snapshots(
            vec![session; MAX_RESTORED_SESSIONS + 1],
            Some(usize::MAX),
            Vec::new(),
            None,
        );
        write_private(&path, serde_json::to_vec(&snapshot).unwrap());

        let (loaded, warnings) = SessionsSnapshot::load_with_warnings(&path).unwrap();

        assert!(!warnings.is_empty());
        assert_eq!(loaded.sessions.len(), MAX_RESTORED_SESSIONS);
        assert_eq!(
            loaded.active_index,
            Some(MAX_RESTORED_SESSIONS.saturating_sub(1))
        );
        assert!(loaded.sessions.iter().all(|session| {
            session.name.len() <= MAX_SESSION_NAME_BYTES
                && session.tags.len() <= MAX_SESSION_TAGS
                && session.session_id.is_none()
        }));
    }

    #[test]
    fn bounded_loader_discards_wide_arrays_before_owning_them() {
        let root = TestDir::new("wide-bounded-load");
        let path = root.0.join("sessions.json");
        let tags = std::iter::repeat_n(r#""tag""#, MAX_SESSION_TAGS + 40)
            .collect::<Vec<_>>()
            .join(",");
        let session = format!(r#"{{"name":"name","tags":[{tags}]}}"#);
        let sessions = std::iter::repeat_n(session, MAX_RESTORED_SESSIONS + 500)
            .collect::<Vec<_>>()
            .join(",");
        write_private(
            &path,
            format!(r#"{{"version":4,"sessions":[{sessions}],"tabs":[]}}"#),
        );

        let (loaded, warnings) = SessionsSnapshot::load_with_warnings(&path).unwrap();

        assert_eq!(loaded.sessions.len(), MAX_RESTORED_SESSIONS);
        assert!(loaded
            .sessions
            .iter()
            .all(|session| session.tags.len() == MAX_SESSION_TAGS));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("restored only the first")));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("session fields")));
    }

    #[test]
    fn deeply_nested_layout_prunes_unsafe_branches_without_losing_sessions() {
        let root = TestDir::new("deep-bounded-layout");
        let path = root.0.join("sessions.json");
        let pane = r#"{"kind":"pane","session_id":"session-a"}"#;
        let mut layout = pane.to_string();
        for _ in 0..=MAX_RESTORED_LAYOUT_DEPTH {
            layout =
                format!(r#"{{"kind":"split","horizontal":true,"first":{layout},"second":{pane}}}"#);
        }
        write_private(
            &path,
            format!(
                r#"{{"version":4,"sessions":[{{"name":"a","tags":[],"session_id":"session-a"}}],"tabs":[{{"root":{layout}}}]}}"#
            ),
        );

        let (loaded, warnings) = SessionsSnapshot::load_with_warnings(&path).unwrap();

        assert_eq!(loaded.sessions.len(), 1);
        assert_eq!(loaded.tabs.len(), 1);
        assert!(matches!(
            &loaded.tabs[0].root,
            LayoutNodeSnapshot::Pane { session_id } if session_id == "session-a"
        ));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("pane layout")));
    }

    #[test]
    fn a_late_oversized_tab_does_not_discard_an_earlier_valid_tab() {
        fn balanced_layout(depth: usize) -> String {
            if depth == 0 {
                return r#"{"kind":"pane","session_id":"session-b"}"#.to_string();
            }
            let child = balanced_layout(depth - 1);
            format!(r#"{{"kind":"split","horizontal":true,"first":{child},"second":{child}}}"#)
        }

        let root = TestDir::new("late-oversized-tab");
        let path = root.0.join("sessions.json");
        let oversized = balanced_layout(7);
        write_private(
            &path,
            format!(
                r#"{{"version":4,"sessions":[{{"name":"a","tags":[],"session_id":"session-a"}},{{"name":"b","tags":[],"session_id":"session-b"}}],"tabs":[{{"root":{{"kind":"pane","session_id":"session-a"}}}},{{"root":{oversized}}}]}}"#
            ),
        );

        let (loaded, warnings) = SessionsSnapshot::load_with_warnings(&path).unwrap();

        assert!(!loaded.tabs.is_empty());
        assert!(matches!(
            &loaded.tabs[0].root,
            LayoutNodeSnapshot::Pane { session_id } if session_id == "session-a"
        ));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("pane layout")));
    }

    #[test]
    fn variant_irrelevant_layout_fields_do_not_spend_restore_budgets() {
        fn balanced_layout(depth: usize) -> String {
            if depth == 0 {
                return r#"{"kind":"pane","session_id":"decoy"}"#.to_string();
            }
            let child = balanced_layout(depth - 1);
            format!(r#"{{"kind":"split","horizontal":true,"first":{child},"second":{child}}}"#)
        }

        let hidden_tree = balanced_layout(6);
        let irrelevant_session_id = "x".repeat(MAX_SESSION_ID_BYTES + 1);
        let json = format!(
            r#"{{
                "version": 4,
                "sessions": [
                    {{"name": "a", "tags": [], "session_id": "session-a"}},
                    {{"name": "b", "tags": [], "session_id": "session-b"}},
                    {{"name": "c", "tags": [], "session_id": "session-c"}}
                ],
                "tabs": [
                    {{"root": {{"first": {hidden_tree}, "kind": "pane",
                               "session_id": "session-a", "horizontal": "ignored"}}}},
                    {{"root": {{"kind": "split", "session_id": "{irrelevant_session_id}",
                               "horizontal": true,
                               "first": {{"kind": "pane", "session_id": "session-b"}},
                               "second": {{"kind": "pane", "session_id": "session-c"}}}}}}
                ]
            }}"#
        );

        let (mut loaded, mut warnings) = decode_bounded_snapshot(&json).unwrap();
        warnings.extend(loaded.sanitize());

        assert_eq!(loaded.tabs.len(), 2);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn a_malformed_late_tab_does_not_discard_an_earlier_valid_tab() {
        let json = r#"{
            "version": 4,
            "sessions": [{"name": "a", "tags": [], "session_id": "session-a"}],
            "tabs": [
                {"root": {"kind": "pane", "session_id": "session-a"}},
                {"root": {"kind": "split", "horizontal": true,
                          "first": {"kind": "pane", "session_id": "session-a"}}}
            ]
        }"#;

        let (mut loaded, mut warnings) = decode_bounded_snapshot(json).unwrap();
        warnings.extend(loaded.sanitize());

        assert_eq!(loaded.tabs.len(), 1);
        assert!(matches!(
            &loaded.tabs[0].root,
            LayoutNodeSnapshot::Pane { session_id } if session_id == "session-a"
        ));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("pane layout")));
    }

    #[test]
    fn cumulative_text_is_charged_during_decode() {
        let json = r#"{
            "version": 4,
            "sessions": [
                {"name": "12345678", "tags": ["abcdefgh"]},
                {"name": "87654321", "tags": ["hgfedcba"]}
            ]
        }"#;

        let error = decode_bounded_snapshot_with_text_budget(json, 20)
            .unwrap_err()
            .to_string();
        assert!(error.contains("cumulative text budget"), "{error}");
    }

    #[test]
    fn surplus_sessions_are_schema_validated_without_being_retained() {
        let retained = std::iter::repeat_n(r#"{"name":"a","tags":[]}"#, MAX_RESTORED_SESSIONS)
            .collect::<Vec<_>>()
            .join(",");
        for invalid_surplus in [
            r#"{"name":"a","tags":[],"cwd":7}"#,
            r#"{"name":"a","tags":[],"cwd":null,"cwd":null}"#,
            r#"{"tags":[]}"#,
        ] {
            let json = format!(r#"{{"version":4,"sessions":[{retained},{invalid_surplus}]}}"#);
            assert!(
                decode_bounded_snapshot(&json).is_err(),
                "accepted invalid surplus session: {invalid_surplus}"
            );
        }
    }

    #[test]
    fn unsupported_version_short_circuits_before_scanning_a_later_payload() {
        // The tail is deliberately malformed. Once a leading future version is
        // known, the borrowed raw payload behind it is never inspected.
        let error = decode_bounded_snapshot(r#"{"version":99,"sessions":["#)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("unsupported session snapshot version 99"),
            "{error}"
        );

        // A postfixed version cannot avoid the envelope scan, but still wins
        // over layout decoding once the valid raw envelope has been collected.
        let error = decode_bounded_snapshot(
            r#"{"sessions":[],"layout":{"kind":"split","horizontal":7},"version":99}"#,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("unsupported session snapshot version 99"),
            "{error}"
        );
    }

    #[test]
    fn required_known_fields_remain_strict() {
        for invalid in [
            r#"{"version":4,"version":4,"sessions":[]}"#,
            r#"{"version":4}"#,
            r#"{"version":4,"sessions":{}}"#,
            r#"{"version":4,"sessions":[{"tags":[]}]}"#,
            r#"{"version":4,"sessions":[{"name":"a","name":"b","tags":[]}]}"#,
            r#"{"version":4,"sessions":[{"name":"a","tags":[]}],"active_index":"zero"}"#,
        ] {
            assert!(
                decode_bounded_snapshot(invalid).is_err(),
                "accepted invalid known field: {invalid}"
            );
        }
    }

    #[test]
    fn tab_decode_is_transactional_and_keeps_valid_neighbors() {
        let json = r#"{
            "version": 4,
            "sessions": [
                {"name": "a", "tags": [], "session_id": "session-a"},
                {"name": "b", "tags": [], "session_id": "session-b"}
            ],
            "tabs": [
                {"root": {"kind": "pane", "session_id": "session-a"}, "pinned": true},
                {"pinned": true},
                {"root": {"kind": "split", "horizontal": true,
                          "first": {"kind": "pane", "session_id": "session-b"}}},
                {"root": {"kind": "pane", "session_id": "session-b"}, "marked": true}
            ],
            "active_tab": 3
        }"#;

        let (mut loaded, mut warnings) = decode_bounded_snapshot(json).unwrap();
        warnings.extend(loaded.sanitize());

        assert_eq!(loaded.tabs.len(), 2);
        assert!(loaded.tabs[0].pinned);
        assert!(loaded.tabs[1].marked);
        assert!(matches!(
            &loaded.tabs[1].root,
            LayoutNodeSnapshot::Pane { session_id } if session_id == "session-b"
        ));
        assert_eq!(loaded.active_tab, Some(1));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("pane layout")));
    }

    #[test]
    fn active_tab_tracks_its_input_identity_across_transactional_discards() {
        let invalid = r#"{"pinned":true}"#;
        let first = r#"{"root":{"kind":"pane","session_id":"session-a"}}"#;
        let second = r#"{"root":{"kind":"pane","session_id":"session-b"}}"#;
        let sessions = r#"{"name":"a","tags":[],"session_id":"session-a"},
                          {"name":"b","tags":[],"session_id":"session-b"}"#;

        let before_active = format!(
            r#"{{"version":4,"sessions":[{sessions}],"tabs":[{invalid},{first},{second}],"active_tab":1}}"#
        );
        let (mut snapshot, _) = decode_bounded_snapshot(&before_active).unwrap();
        snapshot.sanitize();
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(snapshot.active_tab, Some(0));

        let active_itself = format!(
            r#"{{"version":4,"sessions":[{sessions}],"tabs":[{first},{invalid},{second}],"active_tab":1}}"#
        );
        let (mut snapshot, mut warnings) = decode_bounded_snapshot(&active_itself).unwrap();
        warnings.extend(snapshot.sanitize());
        assert_eq!(snapshot.tabs.len(), 2);
        assert_eq!(
            snapshot.active_tab,
            Some(1),
            "the first surviving tab after the discarded active tab wins"
        );
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("active tab index")));
    }

    /// Ember prunes an oversized layout branch instead of dropping its tab, and
    /// the layout-node budget is shared across all tabs (a session may only
    /// ever occupy one pane, so 2N-1 nodes always suffice for a legal
    /// snapshot). A pathological middle tab can therefore spend the whole
    /// shared budget: it is kept after pruning, a later tab is dropped with a
    /// warning, and every session still survives the decode.
    #[test]
    fn a_deep_middle_tab_is_pruned_while_sessions_and_earlier_tabs_survive() {
        let mut deep = r#"{"kind":"pane","session_id":"session-b"}"#.to_string();
        for _ in 0..=MAX_RESTORED_LAYOUT_DEPTH {
            deep = format!(
                r#"{{"kind":"split","horizontal":true,"first":{deep},"second":{{"kind":"pane","session_id":"session-b"}}}}"#
            );
        }
        let json = format!(
            r#"{{"version":4,"sessions":[
                    {{"name":"a","tags":[],"session_id":"session-a"}},
                    {{"name":"b","tags":[],"session_id":"session-b"}},
                    {{"name":"c","tags":[],"session_id":"session-c"}}],
                 "tabs":[
                    {{"root":{{"kind":"pane","session_id":"session-a"}}}},
                    {{"root":{deep}}},
                    {{"root":{{"kind":"pane","session_id":"session-c"}}}}
                 ],"active_tab":0}}"#
        );

        let (mut loaded, mut warnings) = decode_bounded_snapshot(&json).unwrap();
        warnings.extend(loaded.sanitize());

        assert_eq!(loaded.sessions.len(), 3);
        assert!(matches!(
            &loaded.tabs[0].root,
            LayoutNodeSnapshot::Pane { session_id } if session_id == "session-a"
        ));
        // The deep tab is retained after its over-limit branches were pruned;
        // duplicate panes then collapse it onto its one surviving session.
        assert!(matches!(
            &loaded.tabs[1].root,
            LayoutNodeSnapshot::Pane { session_id } if session_id == "session-b"
        ));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("pane layout")));
    }

    #[test]
    fn an_invalid_tabs_field_falls_back_to_the_legacy_layout() {
        let json = r#"{
            "version": 3,
            "sessions": [
                {"name": "a", "tags": [], "session_id": "session-a"},
                {"name": "b", "tags": [], "session_id": "session-b"}
            ],
            "tabs": {"not": "an array"},
            "layout": {
                "root": {"kind": "split", "horizontal": true,
                         "first": {"kind": "pane", "session_id": "session-a"},
                         "second": {"kind": "pane", "session_id": "session-b"}}
            }
        }"#;

        let (mut loaded, mut warnings) = decode_bounded_snapshot(json).unwrap();
        warnings.extend(loaded.sanitize());

        assert_eq!(loaded.tabs.len(), 1);
        assert!(matches!(
            loaded.tabs[0].root,
            LayoutNodeSnapshot::Split { .. }
        ));
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("pane layout")));
    }

    #[test]
    fn every_legal_text_field_fits_the_production_cumulative_budget() {
        let root = TestDir::new("maximum-text-budget");
        let path = root.0.join("sessions.json");
        let sessions = (0..MAX_RESTORED_SESSIONS)
            .map(|index| SessionSnapshot {
                name: "n".repeat(MAX_SESSION_NAME_BYTES),
                tags: vec!["t".repeat(MAX_SESSION_TAG_BYTES); MAX_SESSION_TAGS],
                cwd: Some("c".repeat(MAX_SESSION_CWD_BYTES)),
                session_id: Some(format!("session-{index}")),
                custom_name: Some("u".repeat(MAX_SESSION_NAME_BYTES)),
            })
            .collect::<Vec<_>>();
        let tabs = sessions
            .iter()
            .map(|session| {
                let session_id = session.session_id.clone().unwrap();
                LayoutSnapshot {
                    root: LayoutNodeSnapshot::Pane {
                        session_id: session_id.clone(),
                    },
                    focused_session_id: Some(session_id),
                    pinned: false,
                    marked: false,
                    private_title: false,
                }
            })
            .collect();
        let snapshot = SessionsSnapshot::from_snapshots(sessions, Some(0), tabs, Some(0));

        snapshot.save(&path).unwrap();
        let (loaded, warnings) = SessionsSnapshot::load_with_warnings(&path).unwrap();

        assert_eq!(loaded.sessions.len(), MAX_RESTORED_SESSIONS);
        assert_eq!(loaded.tabs.len(), MAX_RESTORED_SESSIONS);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn saved_display_text_is_canonical_on_its_first_reload() {
        let root = TestDir::new("canonical-display-text");
        let path = root.0.join("sessions.json");
        let snapshot = SessionsSnapshot::from_snapshots(
            vec![SessionSnapshot {
                name: format!("a{}b", " ".repeat(MAX_SESSION_NAME_BYTES - 1)),
                tags: Vec::new(),
                cwd: None,
                session_id: None,
                custom_name: None,
            }],
            Some(0),
            Vec::new(),
            None,
        );

        snapshot.save(&path).unwrap();
        let first_bytes = std::fs::read(&path).unwrap();
        let (loaded, warnings) = SessionsSnapshot::load_with_warnings(&path).unwrap();

        assert_eq!(loaded.sessions[0].name, "a");
        assert!(warnings.is_empty(), "{warnings:?}");
        loaded.save(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), first_bytes);
    }

    #[test]
    fn bounded_loader_keeps_legacy_fields_and_ignores_new_ones() {
        let json = r#"{
            "version": 1,
            "sessions": [{"name": "legacy", "tags": [], "future": [1, 2, 3]}],
            "active_index": 0,
            "future_top_level": {"nested": true}
        }"#;

        let (snapshot, warnings) = decode_bounded_snapshot(json).unwrap();

        assert_eq!(snapshot.version, 1);
        assert_eq!(snapshot.sessions[0].name, "legacy");
        assert_eq!(snapshot.active_index, Some(0));
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn long_unknown_field_names_remain_forward_compatible() {
        let unknown = "x".repeat(4 * 1024);
        let json = format!(
            r#"{{"version":4,"sessions":[{{"name":"kept","tags":[],"{unknown}":true}}],"{unknown}":{{"nested":true}}}}"#
        );

        let (snapshot, warnings) = decode_bounded_snapshot(&json).unwrap();

        assert_eq!(snapshot.sessions[0].name, "kept");
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn bounded_session_names_keep_utf8_boundaries() {
        let bounded = bounded_session_name(&format!("  {}  ", "雪".repeat(200)));
        assert_eq!(bounded, "雪".repeat(85));
        assert_eq!(bounded.len(), 255);
        assert_eq!(bounded_session_name("  short name  "), "short name");
        assert_eq!(
            bounded_session_name("safe\n\u{202e}spoof\u{7f}"),
            "safespoof"
        );
    }

    #[test]
    fn future_snapshot_versions_are_rejected_before_restore() {
        let root = TestDir::new("future-version");
        let path = root.0.join("sessions.json");
        write_private(
            &path,
            br#"{"version":99,"sessions":[{"name":"future","tags":[]}]}"#,
        );

        let error = SessionsSnapshot::load(&path).unwrap_err().to_string();
        assert!(
            error.contains("unsupported session snapshot version 99"),
            "{error}"
        );
        assert!(path.exists());
    }

    #[test]
    fn oversized_snapshot_is_rejected_before_reading_it() {
        let root = TestDir::new("oversized-snapshot");
        let path = root.0.join("sessions.json");
        write_private(&path, vec![b' '; MAX_SESSION_SNAPSHOT_BYTES as usize + 1]);

        let error = SessionsSnapshot::load(&path).unwrap_err().to_string();
        assert!(error.contains("limit"), "{error}");
    }

    /// The seam, not the mechanism (core owns and tests the naming): what this
    /// pins is that ember's loader rejects a corrupt snapshot and that the
    /// bytes survive being moved aside, in that order — startup writes fresh
    /// state right after, and the evidence must already be out of its way.
    #[test]
    fn malformed_snapshot_is_quarantined_without_losing_its_bytes() {
        let root = TestDir::new("corrupt-snapshot");
        let path = root.0.join("sessions.json");
        let original = b"{ definitely not valid JSON";
        write_private(&path, original);

        assert!(SessionsSnapshot::load(&path).is_err());
        let backup = jterm_core::snapshot_file::quarantine_corrupt(&path).unwrap();

        assert!(!path.exists());
        assert_eq!(std::fs::read(backup).unwrap(), original);
    }

    /// A snapshot path that is not a regular file must be rejected instead of
    /// read: an attacker-planted fifo at a configured `session_history_file`
    /// used to hang the whole startup inside `open`.
    #[cfg(unix)]
    #[test]
    fn a_fifo_at_the_snapshot_path_is_refused_rather_than_opened() {
        let root = TestDir::new("fifo-snapshot");
        let path = root.0.join("sessions.json");
        let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `name` is a NUL-terminated path that outlives the call.
        if unsafe { libc::mkfifo(name.as_ptr(), 0o600) } != 0 {
            return; // Some sandboxes forbid mkfifo; nothing to assert then.
        }

        let error = SessionsSnapshot::load(&path).unwrap_err().to_string();
        assert!(error.contains("not a regular file"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_and_hardlink_snapshots_are_not_restored() {
        use std::os::unix::fs::symlink;

        let root = TestDir::new("linked-snapshot");
        let target = root.0.join("target.json");
        let symlink_path = root.0.join("symlink.json");
        let hardlink_path = root.0.join("hardlink.json");
        let snapshot = SessionsSnapshot::from_snapshots(Vec::new(), None, Vec::new(), None);
        write_private(&target, serde_json::to_vec(&snapshot).unwrap());

        symlink(&target, &symlink_path).unwrap();
        assert!(SessionsSnapshot::load(&symlink_path).is_err());

        std::fs::hard_link(&target, &hardlink_path).unwrap();
        let error = SessionsSnapshot::load(&hardlink_path)
            .unwrap_err()
            .to_string();
        assert!(error.contains("hard link"), "{error}");
    }

    #[test]
    fn contending_instance_does_not_truncate_owner_pid() {
        let root = TestDir::new("contending");
        let path = root.0.join("instance.lock");
        write_private(&path, "stale-and-long-owner-value");

        let owner = try_acquire_instance_lock_at(&path)
            .unwrap()
            .expect("first caller should acquire the lock");
        let expected_pid = std::process::id().to_string();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected_pid);

        let contender = try_acquire_instance_lock_at(&path).unwrap();
        assert!(contender.is_none());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            std::process::id().to_string()
        );

        drop(owner);
        let replacement = try_acquire_instance_lock_at(&path)
            .unwrap()
            .expect("lock should be available after its owner drops");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            std::process::id().to_string()
        );
        drop(replacement);
    }

    #[test]
    fn transient_inherited_lock_is_retried_until_exec_style_release() {
        let root = TestDir::new("transient-inheritance");
        let path = root.0.join("instance.lock");
        let owner = try_acquire_instance_lock_at(&path)
            .unwrap()
            .expect("first caller should acquire the lock");
        let inherited = owner.try_clone().unwrap();
        drop(owner);
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            drop(inherited);
        });

        let replacement = try_acquire_instance_lock_at(&path)
            .unwrap()
            .expect("a transient inherited descriptor should be retried");

        releaser.join().unwrap();
        drop(replacement);
    }

    #[cfg(unix)]
    #[test]
    fn forked_child_can_close_its_registered_instance_lock_copy() {
        let root = TestDir::new("fork-inheritance");
        let path = root.0.join("instance.lock");
        let file = try_acquire_instance_lock_at(&path)
            .unwrap()
            .expect("first caller should acquire the lock");
        let owner = InstanceLock::register(file).unwrap();
        let inherited_fd = inherited_instance_lock_fd();
        assert!(inherited_fd >= 0);

        let mut ready_pipe = [-1; 2];
        // SAFETY: ready_pipe points to two writable integers.
        assert_eq!(unsafe { libc::pipe(ready_pipe.as_mut_ptr()) }, 0);
        // SAFETY: both branches below restrict their post-fork work to raw
        // async-signal-safe syscalls until the child exits.
        let child_pid = unsafe { libc::fork() };
        assert!(child_pid >= 0, "fork failed");
        if child_pid == 0 {
            // SAFETY: these descriptors are inherited from the parent and the
            // child never returns into Rust or runs destructors.
            unsafe {
                libc::close(ready_pipe[0]);
                libc::close(inherited_fd);
                let marker = [1u8];
                libc::write(
                    ready_pipe[1],
                    marker.as_ptr().cast::<libc::c_void>(),
                    marker.len(),
                );
                loop {
                    libc::pause();
                }
            }
        }
        let _child = ForkChild(child_pid);

        // SAFETY: the parent owns both pipe descriptors after a successful
        // fork and waits for the child's one-byte close acknowledgement.
        unsafe {
            libc::close(ready_pipe[1]);
            let mut marker = [0u8];
            let read = loop {
                let result = libc::read(
                    ready_pipe[0],
                    marker.as_mut_ptr().cast::<libc::c_void>(),
                    marker.len(),
                );
                if result < 0
                    && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                {
                    continue;
                }
                break result;
            };
            libc::close(ready_pipe[0]);
            assert_eq!(read, 1);
        }

        drop(owner);
        assert_eq!(
            inherited_instance_lock_fd(),
            NO_INSTANCE_LOCK_FD,
            "dropping the parent guard must clear the published descriptor"
        );
        let replacement = try_acquire_instance_lock_at(&path)
            .unwrap()
            .expect("the live forked child must not retain the lock");
        drop(replacement);
    }

    #[cfg(unix)]
    #[test]
    fn instance_lock_symlink_never_changes_its_target() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = TestDir::new("symlink");
        let target = root.0.join("do-not-touch");
        let lock_path = root.0.join("instance.lock");
        write_private(&target, "sentinel contents");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&target, &lock_path).unwrap();

        assert!(try_acquire_instance_lock_at(&lock_path).is_err());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "sentinel contents"
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[cfg(unix)]
    #[test]
    fn instance_lock_hard_link_never_changes_its_target() {
        let root = TestDir::new("hard-link");
        let target = root.0.join("do-not-touch");
        let lock_path = root.0.join("instance.lock");
        write_private(&target, "sentinel contents");
        std::fs::hard_link(&target, &lock_path).unwrap();

        assert!(try_acquire_instance_lock_at(&lock_path).is_err());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "sentinel contents"
        );
    }

    #[cfg(unix)]
    #[test]
    fn instance_lock_fifo_is_rejected_without_blocking() {
        use std::ffi::CString;

        let root = TestDir::new("fifo");
        let lock_path = root.0.join("instance.lock");
        let encoded = CString::new(lock_path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: encoded is a live NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);

        let started = std::time::Instant::now();
        assert!(try_acquire_instance_lock_at(&lock_path).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }
}
