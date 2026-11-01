//! Revisioned bridge between the native chat store and the shared workspace host.
//! UI selection never replaces the store or an unrelated in-flight request.
use jterm_core::agent_workspace::{
    ConversationId, ConversationKind, StoredChat, StoredMessage, SubmissionReceipt,
    WorkspaceClient, WorkspaceCommand as Command, WorkspaceEvent, WorkspaceReply, WorkspaceRow,
};
use jterm_core::ai::{ChatStore, ConversationSnapshot, RequestToken};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ChatRef {
    pub(crate) chat_id: u64,
    selection: u64,
}

pub(crate) enum Effect {
    Activate(u64),
    SyncDraft(u64),
    Submit {
        chat_id: u64,
        text: String,
        clear_draft: bool,
    },
}

enum Pending {
    Refresh,
    Import {
        snapshots: HashMap<u64, String>,
    },
    History {
        chat_id: u64,
        initial: bool,
    },
    Open {
        selection: u64,
        keep_local: bool,
    },
    Save {
        chat_id: u64,
        generation: u64,
        submit: Option<String>,
    },
    Draft {
        chat_id: u64,
        generation: u64,
    },
    Begin {
        chat_id: u64,
        text: String,
        generation: u64,
    },
    Finish {
        chat_id: u64,
    },
    Delete {
        chat_id: u64,
    },
    Read,
}

struct KnownChat {
    stored: StoredChat,
    generation: u64,
    dirty: bool,
    saving: bool,
    blocked: bool,
    changed: Instant,
    submit: Option<String>,
    /// Latest local draft accepted for synchronization. The automatic clear
    /// after BeginSubmission does not create a newer user-authored draft.
    observed_draft: String,
    latest_draft: String,
}

pub(crate) struct WorkspaceChats {
    client: WorkspaceClient,
    pending: HashMap<u64, Pending>,
    known: HashMap<u64, KnownChat>,
    awaiting_dispatch: HashMap<u64, SubmissionReceipt>,
    receipts: HashMap<RequestToken, SubmissionReceipt>,
    pub(crate) rows: Vec<WorkspaceRow>,
    pub(crate) notice: Option<String>,
    imported: bool,
    selection: u64,
    last_refresh: Instant,
    read_pending: HashMap<u64, (f64, Instant)>,
    history: HashMap<u64, HistoryPage>,
    cancelled_dispatch: HashSet<u64>,
}

#[derive(Default)]
struct HistoryPage {
    messages: Vec<StoredMessage>,
    cursor: Option<u64>,
    initialized: bool,
    loading: bool,
}

impl Default for WorkspaceChats {
    fn default() -> Self {
        Self {
            client: WorkspaceClient::new(),
            pending: HashMap::new(),
            known: HashMap::new(),
            awaiting_dispatch: HashMap::new(),
            receipts: HashMap::new(),
            rows: Vec::new(),
            notice: None,
            imported: false,
            selection: 0,
            last_refresh: Instant::now() - Duration::from_secs(10),
            read_pending: HashMap::new(),
            history: HashMap::new(),
            cancelled_dispatch: HashSet::new(),
        }
    }
}

impl WorkspaceChats {
    fn send(&mut self, command: Command, pending: Pending) -> bool {
        match self.client.send(command) {
            Ok(request) => {
                self.pending.insert(request, pending);
                true
            }
            Err(error) => {
                self.notice = Some(error);
                false
            }
        }
    }

    pub(crate) fn start(&mut self, store: &mut ChatStore, namespace: &str, redact: bool) {
        if self.imported {
            return;
        }
        self.imported = true;
        // Shared IDs occupy a disjoint numeric range and must never be imported
        // as another legacy conversation when a local safety snapshot is restored.
        if let Ok((snapshot, _)) = store.snapshot_for_persistence(redact) {
            let legacy = snapshot
                .chats()
                .iter()
                .filter(|chat| chat.id() < (1u64 << 48))
                .cloned()
                .collect::<Vec<_>>();
            let snapshots = legacy
                .iter()
                .filter_map(|chat| {
                    ConversationSnapshot::from_chats(chat.id(), vec![chat.clone()])
                        .and_then(|snapshot| snapshot.to_json().ok())
                        .map(|json| (chat.id(), json))
                })
                .collect();
            if let Some(first) = legacy.first() {
                if let Some(snapshot) = ConversationSnapshot::from_chats(first.id(), legacy) {
                    if let Ok(snapshot_json) = snapshot.to_json() {
                        self.send(
                            Command::ImportChats {
                                namespace: namespace.into(),
                                snapshot_json,
                            },
                            Pending::Import { snapshots },
                        );
                    }
                }
            }
        }
        self.refresh();
    }

    pub(crate) fn pin(&mut self, id: ConversationId, pinned: bool) {
        if self.send(Command::Pin { id, pinned }, Pending::Read) {
            self.refresh();
        }
    }

    pub(crate) fn refresh(&mut self) {
        if self
            .pending
            .values()
            .any(|pending| matches!(pending, Pending::Refresh))
        {
            return;
        }
        self.last_refresh = Instant::now();
        self.send(Command::Refresh, Pending::Refresh);
    }

    pub(crate) fn open(&mut self, id: ConversationId) {
        self.selection = self.selection.wrapping_add(1);
        self.send(
            Command::LoadChat { id },
            Pending::Open {
                selection: self.selection,
                keep_local: false,
            },
        );
    }

    pub(crate) fn reload_shared_draft(&mut self, id: ConversationId) {
        for known in self
            .known
            .values_mut()
            .filter(|known| known.stored.row.id == id)
        {
            known.dirty = false;
            known.blocked = false;
            known.submit = None;
        }
        self.open(id);
    }

    pub(crate) fn reload_keep_local(&mut self, id: ConversationId) {
        self.selection = self.selection.wrapping_add(1);
        self.send(
            Command::LoadChat { id },
            Pending::Open {
                selection: self.selection,
                keep_local: true,
            },
        );
    }

    pub(crate) fn create(&mut self) {
        self.selection = self.selection.wrapping_add(1);
        self.send(
            Command::CreateChat {
                title: "New chat".into(),
            },
            Pending::Open {
                selection: self.selection,
                keep_local: false,
            },
        );
    }

    pub(crate) fn reference(&self, chat_id: u64) -> ChatRef {
        ChatRef {
            chat_id,
            selection: self.selection,
        }
    }
    pub(crate) fn accepts(&self, reference: ChatRef, active: u64) -> bool {
        reference.selection == self.selection && reference.chat_id == active && !self.opening()
    }
    pub(crate) fn cancel_queued(&mut self, id: u64) -> bool {
        if !self.is_pending(id) {
            return false;
        }
        if self
            .pending
            .values()
            .any(|pending| matches!(pending, Pending::Begin { chat_id, .. } if *chat_id == id))
        {
            self.cancelled_dispatch.insert(id);
        }
        if let Some(chat) = self.known.get_mut(&id) {
            chat.submit = None;
            chat.generation = chat.generation.wrapping_add(1);
            chat.dirty = true;
        }
        self.notice =
            Some("Submission cancelled before provider dispatch. Your draft was kept.".into());
        true
    }
    pub(crate) fn opening(&self) -> bool {
        self.pending.values().any(|pending| matches!(pending, Pending::Open { selection, .. } if *selection == self.selection))
    }
    pub(crate) fn leave(&mut self) {
        self.selection = self.selection.wrapping_add(1);
    }
    pub(crate) fn is_shared(&self, id: u64) -> bool {
        self.known.contains_key(&id)
    }
    pub(crate) fn id_for(&self, id: u64) -> Option<&ConversationId> {
        self.known.get(&id).map(|chat| &chat.stored.row.id)
    }
    pub(crate) fn is_pending(&self, id: u64) -> bool {
        self.known.get(&id).is_some_and(|chat| chat.submit.is_some())
            || self.pending.values().any(|pending| matches!(pending, Pending::Begin { chat_id, .. } | Pending::Save {chat_id, submit:Some(_), ..} if *chat_id == id))
    }
    pub(crate) fn has_work(&self) -> bool {
        !self.pending.is_empty()
            || !self.read_pending.is_empty()
            || self.known.values().any(|chat| chat.dirty && !chat.blocked)
    }

    pub(crate) fn older_messages(&self, id: u64) -> &[StoredMessage] {
        self.history
            .get(&id)
            .map_or(&[], |page| page.messages.as_slice())
    }
    pub(crate) fn can_load_history(&self, id: u64) -> bool {
        self.known
            .get(&id)
            .is_some_and(|known| known.stored.has_older)
            && self
                .history
                .get(&id)
                .is_none_or(|page| !page.initialized || page.cursor.is_some())
    }
    pub(crate) fn history_loading(&self, id: u64) -> bool {
        self.history.get(&id).is_some_and(|page| page.loading)
    }
    pub(crate) fn load_history(&mut self, id: u64) {
        if !self.can_load_history(id) || self.history_loading(id) {
            return;
        }
        let remote = self.known[&id].stored.row.id.clone();
        let page = self.history.entry(id).or_default();
        let initial = !page.initialized;
        let before = page.cursor;
        page.loading = true;
        if !self.send(
            Command::History {
                id: remote,
                before,
                limit: 100,
            },
            Pending::History {
                chat_id: id,
                initial,
            },
        ) {
            if let Some(page) = self.history.get_mut(&id) {
                page.loading = false;
            }
        }
    }

    pub(crate) fn read_position(&self, id: u64) -> Option<f64> {
        self.known
            .get(&id)?
            .stored
            .read_position
            .as_ref()
            .filter(|position| position.view == "api-transcript-relative")
            .map(|position| position.position.clamp(0.0, 1.0))
    }

    pub(crate) fn remember_read(&mut self, id: u64, position: f64) {
        if !position.is_finite() || !self.known.contains_key(&id) {
            return;
        }
        let position = position.clamp(0.0, 1.0);
        let previous = self
            .read_pending
            .get(&id)
            .map(|(position, _)| *position)
            .or_else(|| self.read_position(id));
        if previous.is_some_and(|old| (old - position).abs() < 0.002) {
            return;
        }
        self.read_pending.insert(id, (position, Instant::now()));
    }

    pub(crate) fn changed(&mut self, store: &ChatStore) {
        let id = store.active_id();
        if let Some(chat) = self.known.get_mut(&id) {
            chat.generation = chat.generation.wrapping_add(1);
            chat.dirty = true;
            chat.changed = Instant::now();
            chat.latest_draft = store.active_draft().to_owned();
        }
    }

    pub(crate) fn changed_id(&mut self, store: &ChatStore, id: u64) {
        if let Some(chat) = self.known.get_mut(&id) {
            chat.generation = chat.generation.wrapping_add(1);
            chat.dirty = true;
            chat.changed = Instant::now();
            if let Some(snapshot) = store.snapshot_chat_for_persistence(id, false) {
                chat.latest_draft = snapshot.draft().into();
            }
        }
    }

    pub(crate) fn delete(&mut self, id: u64) -> bool {
        let Some(known) = self.known.get(&id) else {
            return false;
        };
        if known.saving
            || known.dirty
            || self.receipts.keys().any(|token| token.chat_id == id)
            || self.is_pending(id)
        {
            self.notice =
                Some("Wait for this conversation to finish saving before deleting it.".into());
            return false;
        }
        self.send(
            Command::Delete {
                id: known.stored.row.id.clone(),
            },
            Pending::Delete { chat_id: id },
        )
    }

    pub(crate) fn submit(&mut self, store: &ChatStore, text: String) -> bool {
        let id = store.active_id();
        if text.trim().is_empty() || store.is_active_busy() || self.is_pending(id) {
            return false;
        }
        let Some(chat) = self.known.get_mut(&id) else {
            self.notice =
                Some("Open this conversation from the shared sidebar before sending.".into());
            return false;
        };
        if chat.blocked {
            self.notice = Some(
                "Reload the shared conversation before sending. Your local draft is kept.".into(),
            );
            return false;
        }
        chat.generation = chat.generation.wrapping_add(1);
        chat.dirty = true;
        chat.latest_draft = store.active_draft().to_owned();
        chat.submit = Some(text);
        chat.changed = Instant::now() - Duration::from_secs(1);
        true
    }

    pub(crate) fn bind(&mut self, token: RequestToken, clear_draft: bool) {
        if let Some(receipt) = self.awaiting_dispatch.remove(&token.chat_id) {
            if let Some(chat) = self.known.get_mut(&token.chat_id) {
                if clear_draft {
                    chat.observed_draft.clear();
                    chat.latest_draft.clear();
                    chat.dirty = false;
                } else {
                    chat.dirty = true;
                    chat.changed = Instant::now() - Duration::from_secs(1);
                }
            }
            self.receipts.insert(token, receipt);
        }
    }
    pub(crate) fn refuse_dispatch(&mut self, chat_id: u64) {
        if let Some(receipt) = self.awaiting_dispatch.remove(&chat_id) {
            self.send(
                Command::CancelSubmission {
                    receipt,
                    restore_draft: true,
                },
                Pending::Finish { chat_id },
            );
        }
    }
    pub(crate) fn complete(&mut self, token: RequestToken, text: &str) {
        if let Some(receipt) = self.receipts.remove(&token) {
            self.send(
                Command::CompleteSubmission {
                    receipt,
                    text: text.to_owned(),
                },
                Pending::Finish {
                    chat_id: token.chat_id,
                },
            );
        }
    }
    pub(crate) fn cancel(&mut self, token: RequestToken) {
        if let Some(receipt) = self.receipts.remove(&token) {
            self.send(
                Command::CancelSubmission {
                    receipt,
                    restore_draft: true,
                },
                Pending::Finish {
                    chat_id: token.chat_id,
                },
            );
        }
    }

    fn handle_reply(
        &mut self,
        store: &mut ChatStore,
        request_id: u64,
        result: Result<WorkspaceReply, String>,
        redact: bool,
    ) -> Vec<Effect> {
        let mut effects = Vec::new();
        let Some(pending) = self.pending.remove(&request_id) else {
            return effects;
        };
        let reply = match result {
            Ok(reply) => reply,
            Err(error) => {
                match &pending {
                    Pending::Save { chat_id, .. }
                    | Pending::Draft { chat_id, .. }
                    | Pending::Begin { chat_id, .. }
                    | Pending::Finish { chat_id } => {
                        if let Some(chat) = self.known.get_mut(chat_id) {
                            chat.saving = false;
                            chat.submit = None;
                            chat.dirty = true;
                            chat.blocked = true;
                        }
                    }
                    Pending::History { chat_id, .. } => {
                        if let Some(page) = self.history.get_mut(chat_id) {
                            page.loading = false;
                        }
                    }
                    _ => {}
                }
                self.notice = Some(format!(
                    "Shared conversation: {error}. Your local draft has been kept."
                ));
                return effects;
            }
        };
        match (pending, reply) {
            (Pending::Refresh, WorkspaceReply::Catalog { rows }) => {
                self.rows = rows
                    .into_iter()
                    .filter(|row| row.kind == ConversationKind::Api)
                    .collect();
                self.rows.sort_by(|a, b| {
                    b.pinned
                        .cmp(&a.pinned)
                        .then(b.updated_at_ms.cmp(&a.updated_at_ms))
                        .then(a.id.cmp(&b.id))
                });
            }
            (Pending::Import { snapshots }, WorkspaceReply::Imported { mappings }) => {
                // Import ACK plus exact snapshot equality is the proof required
                // before retiring a legacy GUI cache row. A later draft survives.
                for mapping in mappings {
                    if let Some(before) = snapshots.get(&mapping.legacy_id) {
                        let now = store
                            .snapshot_chat_for_persistence(mapping.legacy_id, redact)
                            .and_then(|chat| {
                                ConversationSnapshot::from_chats(chat.id(), vec![chat])
                            })
                            .and_then(|snapshot| snapshot.to_json().ok());
                        if now.as_ref() == Some(before) {
                            let _ = store.evict_imported_legacy_chat(mapping.legacy_id);
                        }
                    }
                }
                self.refresh();
            }
            (
                Pending::Open {
                    selection,
                    keep_local,
                },
                WorkspaceReply::Chat { chat },
            ) => {
                let Ok(snapshot) = ConversationSnapshot::from_json(&chat.snapshot_json) else {
                    return effects;
                };
                let Some(row) = snapshot.active_chat().cloned() else {
                    return effects;
                };
                let id = row.id();
                // Refreshing a busy row must not revoke its stream or epoch.
                if store
                    .in_flight_tokens()
                    .iter()
                    .any(|token| token.chat_id == id)
                {
                    if selection == self.selection {
                        effects.push(Effect::Activate(id));
                    }
                    return effects;
                }
                let local_draft = self
                    .known
                    .get(&id)
                    .filter(|known| known.dirty)
                    .and_then(|_| {
                        store
                            .snapshot_chat_for_persistence(id, false)
                            .map(|snapshot| snapshot.draft().to_owned())
                    });
                if store.at_capacity() && store.snapshot_chat_for_persistence(id, false).is_none() {
                    let candidates: Vec<_> = self
                        .known
                        .iter()
                        .filter(|(&cached, known)| {
                            cached != store.active_id()
                                && !known.dirty
                                && !known.saving
                                && !self.is_pending(cached)
                                && !self.receipts.keys().any(|token| token.chat_id == cached)
                        })
                        .map(|(&cached, _)| cached)
                        .collect();
                    for cached in candidates {
                        if store.evict_cached_chat(cached) == Ok(true) {
                            self.known.remove(&cached);
                            self.history.remove(&cached);
                            break;
                        }
                    }
                }
                match store.upsert_persisted_chat(row.clone()) {
                    Ok(_) => {
                        self.known.insert(
                            id,
                            KnownChat {
                                stored: chat,
                                generation: 0,
                                dirty: false,
                                saving: false,
                                blocked: false,
                                changed: Instant::now(),
                                submit: None,
                                observed_draft: row.draft().into(),
                                latest_draft: row.draft().into(),
                            },
                        );
                        if let Some(draft) = local_draft {
                            let selected = store.active_id();
                            store.select_chat(id);
                            store.set_active_draft(draft);
                            self.changed(store);
                            store.select_chat(selected);
                            let remote = row.draft();
                            let comparable = if redact {
                                jterm_core::redact::redact_secrets(
                                    store.chat_draft(id).unwrap_or_default(),
                                )
                            } else {
                                store.chat_draft(id).unwrap_or_default().to_owned()
                            };
                            if !keep_local && comparable != remote {
                                if let Some(known) = self.known.get_mut(&id) {
                                    known.blocked = true;
                                }
                                self.notice = Some("Your local draft and a different shared draft are retained. Choose Keep my draft to continue with the local text.".into());
                            } else {
                                self.notice = Some(
                                    "Shared history refreshed; your local draft was kept.".into(),
                                );
                            }
                        }
                        if selection == self.selection {
                            effects.push(Effect::Activate(id));
                        }
                    }
                    Err(error) => {
                        self.notice = Some(format!("Could not open shared conversation: {error:?}"))
                    }
                }
            }
            (
                Pending::Save {
                    chat_id,
                    generation,
                    submit,
                },
                WorkspaceReply::Chat { chat },
            ) => {
                let mut begin = None;
                if let Some(known) = self.known.get_mut(&chat_id) {
                    known.saving = false;
                    known.stored = chat.clone();
                    if known.generation == generation {
                        known.dirty = false;
                        known.submit = None;
                        if let Some(text) = submit {
                            begin = Some((
                                Command::BeginSubmission {
                                    id: chat.row.id.clone(),
                                    expected_revision: chat.revision,
                                    draft_revision: chat.draft_revision,
                                    text: if redact {
                                        jterm_core::redact::redact_secrets(&text)
                                    } else {
                                        text.clone()
                                    },
                                },
                                text.clone(),
                            ));
                            known.observed_draft = text;
                        }
                    } else if submit.is_some() {
                        known.submit = None;
                        self.notice = Some(
                            "The draft changed while saving. Review it and press Send again."
                                .into(),
                        );
                    }
                }
                if let Some((command, text)) = begin {
                    self.send(
                        command,
                        Pending::Begin {
                            chat_id,
                            text,
                            generation,
                        },
                    );
                }
            }
            (
                Pending::Draft {
                    chat_id,
                    generation,
                },
                WorkspaceReply::Chat { chat },
            ) => {
                if let Some(known) = self.known.get_mut(&chat_id) {
                    known.stored = chat;
                    known.saving = false;
                    if known.generation == generation {
                        known.dirty = false;
                    }
                }
            }
            (
                Pending::Begin {
                    chat_id,
                    text,
                    generation,
                },
                WorkspaceReply::Submission { receipt },
            ) => {
                let Some(known) = self.known.get(&chat_id) else {
                    return effects;
                };
                if receipt.id != known.stored.row.id {
                    self.notice = Some(
                        "Shared submission identity changed; no provider request was sent.".into(),
                    );
                    return effects;
                }
                if self.cancelled_dispatch.remove(&chat_id) {
                    self.send(
                        Command::CancelSubmission {
                            receipt,
                            restore_draft: true,
                        },
                        Pending::Finish { chat_id },
                    );
                    return effects;
                }
                let clear_draft = known.generation == generation;
                if let Some(known) = self.known.get_mut(&chat_id) {
                    known.stored.revision += 1;
                    known.stored.epoch = receipt.epoch;
                }
                self.awaiting_dispatch.insert(chat_id, receipt);
                effects.push(Effect::Submit {
                    chat_id,
                    text,
                    clear_draft,
                });
            }
            (Pending::Delete { chat_id }, WorkspaceReply::Ack) => {
                if let Some(known) = self.known.remove(&chat_id) {
                    self.rows.retain(|row| row.id != known.stored.row.id);
                }
                let selected = store.active_id();
                if store.select_chat(chat_id) {
                    let _ = store.delete_active();
                    if selected == chat_id {
                        if self.is_shared(store.active_id()) {
                            effects.push(Effect::Activate(store.active_id()));
                        } else if let Some(next) = self.rows.iter().find(|row| !row.archived) {
                            self.open(next.id.clone());
                        } else {
                            self.create();
                        }
                    } else {
                        store.select_chat(selected);
                    }
                }
                self.refresh();
            }
            (
                Pending::History { chat_id, initial },
                WorkspaceReply::Messages {
                    mut messages,
                    next_before,
                },
            ) => {
                let mut load_next = false;
                if initial {
                    let recent = self.known.get(&chat_id).and_then(|known| {
                        ConversationSnapshot::from_json(&known.stored.snapshot_json).ok()
                    });
                    let turns = recent
                        .as_ref()
                        .map(|snapshot| snapshot.turns())
                        .unwrap_or_default();
                    let overlap = turns.len().min(messages.len());
                    let matches = turns.len() <= messages.len()
                        && messages[messages.len() - overlap..].iter().zip(turns).all(
                            |(message, turn)| {
                                message.text == turn.text
                                    && (message.role == "user")
                                        == (turn.role == jterm_core::ai::Role::User)
                            },
                        );
                    if !matches {
                        self.notice = Some("History changed while loading. Reload this conversation before browsing earlier messages.".into());
                        if let Some(page) = self.history.get_mut(&chat_id) {
                            page.loading = false;
                        }
                        return effects;
                    }
                    messages.truncate(messages.len() - overlap);
                }
                let page = self.history.entry(chat_id).or_default();
                page.loading = false;
                page.initialized = true;
                page.cursor = next_before;
                if !messages.is_empty() {
                    messages.append(&mut page.messages);
                    page.messages = messages;
                } else if initial && page.cursor.is_some() {
                    load_next = true;
                }
                if load_next {
                    self.load_history(chat_id);
                }
            }
            (Pending::Finish { chat_id }, WorkspaceReply::Chat { chat }) => {
                let remote_draft = ConversationSnapshot::from_json(&chat.snapshot_json)
                    .ok()
                    .and_then(|snapshot| snapshot.active_chat().map(|row| row.draft().to_owned()));
                let local_draft = store.chat_draft(chat_id).map(str::to_owned);
                let Some(known) = self.known.get_mut(&chat_id) else {
                    return effects;
                };
                if known.stored.row.id != chat.row.id {
                    self.notice = Some(
                        "Reply belongs to a different conversation; local state was retained."
                            .into(),
                    );
                    known.blocked = true;
                    known.saving = false;
                    return effects;
                }
                known.saving = false;
                known.stored = chat;
                let (Some(remote), Some(local)) = (remote_draft, local_draft) else {
                    known.blocked = true;
                    self.notice = Some("The completed turn could not reconcile its saved draft. Your local state is retained.".into());
                    return effects;
                };
                let comparable = if redact {
                    jterm_core::redact::redact_secrets(&local)
                } else {
                    local.clone()
                };
                if !local.is_empty() && comparable != remote {
                    // A newer draft may have been authored in another window.
                    // Never bless our old local text with the returned revision.
                    known.blocked = true;
                    known.dirty = true;
                    self.notice = Some("The completed turn has a different shared draft. Your local draft and the shared draft are both retained; reload before sending.".into());
                } else {
                    if local.is_empty() && !remote.is_empty() {
                        if store.set_chat_draft(chat_id, remote.clone()) {
                            effects.push(Effect::SyncDraft(chat_id));
                        }
                        known.latest_draft = remote.clone();
                        known.observed_draft = remote;
                    }
                    known.dirty = true;
                    known.generation = known.generation.wrapping_add(1);
                    known.changed = Instant::now();
                }
                self.refresh();
            }
            _ => {}
        }
        effects
    }

    pub(crate) fn drive(&mut self, store: &mut ChatStore, redact: bool) -> Vec<Effect> {
        let mut effects = Vec::new();
        for WorkspaceEvent::Reply { request_id, result } in self.client.poll() {
            effects.extend(self.handle_reply(store, request_id, result, redact));
        }
        let due: Vec<_> = self
            .known
            .iter()
            .filter(|(_, chat)| {
                chat.dirty
                    && !chat.blocked
                    && !chat.saving
                    && chat.changed.elapsed() >= Duration::from_millis(350)
            })
            .map(|(&id, _)| id)
            .collect();
        for id in due {
            if self.awaiting_dispatch.contains_key(&id)
                || self.pending.values().any(
                    |pending| matches!(pending, Pending::Begin { chat_id, .. } if *chat_id == id),
                )
            {
                continue;
            }
            let Some(snapshot) = store.snapshot_chat_for_persistence(id, redact) else {
                continue;
            };
            let is_busy = self.receipts.keys().any(|token| token.chat_id == id);
            let known = self.known.get_mut(&id).unwrap();
            let generation = known.generation;
            let (command, pending) = if is_busy {
                // Use the actual current draft, not durable_draft's failed-send
                // recovery payload, while a provider turn is in progress.
                let text = known.latest_draft.clone();
                if text == known.observed_draft {
                    known.dirty = false;
                    continue;
                }
                known.observed_draft = text.clone();
                (
                    Command::SetDraft {
                        id: known.stored.row.id.clone(),
                        expected_revision: known.stored.draft_revision,
                        text: if redact {
                            jterm_core::redact::redact_secrets(&text)
                        } else {
                            text
                        },
                    },
                    Pending::Draft {
                        chat_id: id,
                        generation,
                    },
                )
            } else {
                let Some(one) = ConversationSnapshot::from_chats(id, vec![snapshot]) else {
                    continue;
                };
                let Ok(snapshot_json) = one.to_json() else {
                    continue;
                };
                (
                    Command::SaveChat {
                        id: known.stored.row.id.clone(),
                        expected_revision: known.stored.revision,
                        snapshot_json,
                    },
                    Pending::Save {
                        chat_id: id,
                        generation,
                        submit: known.submit.clone(),
                    },
                )
            };
            known.saving = true;
            if !self.send(command, pending) {
                if let Some(known) = self.known.get_mut(&id) {
                    known.saving = false;
                }
            }
        }
        let readings: Vec<_> = self
            .read_pending
            .iter()
            .filter(|(_, (_, at))| at.elapsed() >= Duration::from_millis(650))
            .map(|(&id, &(position, _))| (id, position))
            .collect();
        for (id, position) in readings {
            self.read_pending.remove(&id);
            if let Some(known) = self.known.get_mut(&id) {
                let remote = known.stored.row.id.clone();
                known.stored.read_position = Some(jterm_core::agent_workspace::ReadPosition {
                    view: "api-transcript-relative".into(),
                    position,
                    updated_at_ms: 0,
                });
                self.send(
                    Command::SetReadPosition {
                        id: remote,
                        view: "api-transcript-relative".into(),
                        position,
                    },
                    Pending::Read,
                );
            }
        }
        if self.last_refresh.elapsed() >= Duration::from_secs(5) {
            self.refresh();
        }
        effects
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jterm_core::agent_workspace::RuntimeState;
    use jterm_core::ai::{ChatSnapshot, Role, Turn};

    fn stored(id: u64, remote: &str, draft: &str) -> StoredChat {
        let snapshot = ChatSnapshot::from_completed_history(
            id,
            "Review the project",
            false,
            &[
                Turn {
                    role: Role::User,
                    text: "first question".into(),
                },
                Turn {
                    role: Role::Assistant,
                    text: "first answer".into(),
                },
            ],
            None,
            draft,
        );
        StoredChat {
            row: WorkspaceRow {
                id: ConversationId(remote.into()),
                kind: ConversationKind::Api,
                title: "Review the project".into(),
                provider: "API".into(),
                cwd: String::new(),
                preview: "first answer".into(),
                updated_at_ms: 1,
                archived: false,
                pinned: false,
                tags: vec![],
                runtime: RuntimeState::Idle,
            },
            revision: 3,
            draft_revision: 2,
            epoch: 0,
            snapshot_json: ConversationSnapshot::from_chats(id, vec![snapshot])
                .unwrap()
                .to_json()
                .unwrap(),
            has_older: false,
            read_position: None,
        }
    }

    fn loaded() -> (WorkspaceChats, ChatStore, u64) {
        let id = (1u64 << 48) + 91;
        let path =
            std::env::temp_dir().join(format!("jterm-ui-bridge-test-{}", uuid::Uuid::new_v4()));
        let mut bridge = WorkspaceChats {
            client: WorkspaceClient::for_state_dir(path),
            ..Default::default()
        };
        let mut store = ChatStore::default();
        bridge.selection = 1;
        bridge.pending.insert(
            100,
            Pending::Open {
                selection: 1,
                keep_local: false,
            },
        );
        let effects = bridge.handle_reply(
            &mut store,
            100,
            Ok(WorkspaceReply::Chat {
                chat: stored(id, "jterm:api:original", "Question"),
            }),
            false,
        );
        assert!(matches!(effects.as_slice(), [Effect::Activate(found)] if *found == id));
        assert!(store.select_chat(id));
        (bridge, store, id)
    }

    fn acknowledge_save(bridge: &mut WorkspaceChats, store: &mut ChatStore, id: u64) -> u64 {
        assert!(bridge.submit(store, "Question".into()));
        let generation = bridge.known[&id].generation;
        bridge.pending.insert(
            200,
            Pending::Save {
                chat_id: id,
                generation,
                submit: Some("Question".into()),
            },
        );
        let mut saved = bridge.known[&id].stored.clone();
        saved.revision += 1;
        let effects =
            bridge.handle_reply(store, 200, Ok(WorkspaceReply::Chat { chat: saved }), false);
        assert!(
            effects.is_empty(),
            "a saved snapshot is not permission to dispatch HTTP"
        );
        *bridge
            .pending
            .iter()
            .find(|(_, pending)| matches!(pending, Pending::Begin { .. }))
            .unwrap()
            .0
    }

    #[test]
    fn provider_dispatch_waits_for_the_exact_shared_submission_receipt() {
        let (mut bridge, mut store, id) = loaded();
        let request = acknowledge_save(&mut bridge, &mut store, id);
        assert!(bridge.awaiting_dispatch.is_empty());
        let receipt = SubmissionReceipt {
            id: ConversationId("jterm:api:original".into()),
            epoch: 1,
            nonce: "one-owner".into(),
            draft_revision: 2,
        };
        let effects = bridge.handle_reply(
            &mut store,
            request,
            Ok(WorkspaceReply::Submission { receipt }),
            false,
        );
        assert!(
            matches!(effects.as_slice(), [Effect::Submit { chat_id, text, clear_draft: true }] if *chat_id == id && text == "Question")
        );
        let token = store
            .begin_turn("Question".into(), None, "Thinking".into(), true)
            .unwrap()
            .token;
        bridge.bind(token, true);
        store.select_chat(1);
        bridge.complete(token, "final response");
        assert!(bridge
            .pending
            .values()
            .any(|pending| matches!(pending, Pending::Finish {chat_id} if *chat_id == id)));
        assert_eq!(
            store.active_id(),
            1,
            "the response destination never follows the visible chat"
        );
    }

    #[test]
    fn a_newer_draft_survives_an_older_submission_acknowledgement() {
        let (mut bridge, mut store, id) = loaded();
        let request = acknowledge_save(&mut bridge, &mut store, id);
        store.set_active_draft("my next question".into());
        bridge.changed(&store);
        let effects = bridge.handle_reply(
            &mut store,
            request,
            Ok(WorkspaceReply::Submission {
                receipt: SubmissionReceipt {
                    id: ConversationId("jterm:api:original".into()),
                    epoch: 1,
                    nonce: "old-submission".into(),
                    draft_revision: 2,
                },
            }),
            false,
        );
        assert!(matches!(
            effects.as_slice(),
            [Effect::Submit {
                clear_draft: false,
                ..
            }]
        ));
        assert_eq!(store.active_draft(), "my next question");
        let token = store
            .begin_turn("Question".into(), None, "Thinking".into(), true)
            .unwrap()
            .token;
        bridge.bind(token, false);
        assert!(
            bridge.known[&id].dirty,
            "the newer draft must receive its own revision"
        );
    }

    #[test]
    fn foreign_receipts_and_failed_saves_never_dispatch() {
        let (mut bridge, mut store, id) = loaded();
        let request = acknowledge_save(&mut bridge, &mut store, id);
        let effects = bridge.handle_reply(
            &mut store,
            request,
            Ok(WorkspaceReply::Submission {
                receipt: SubmissionReceipt {
                    id: ConversationId("jterm:api:different".into()),
                    epoch: 1,
                    nonce: "wrong".into(),
                    draft_revision: 2,
                },
            }),
            false,
        );
        assert!(effects.is_empty());
        assert!(bridge.awaiting_dispatch.is_empty());
        bridge.pending.insert(
            300,
            Pending::Save {
                chat_id: id,
                generation: 0,
                submit: Some("Question".into()),
            },
        );
        assert!(bridge
            .handle_reply(&mut store, 300, Err("Revision conflict".into()), false)
            .is_empty());
        assert_eq!(store.active_draft(), "Question");
        assert!(bridge.known[&id].blocked);
        assert!(
            !bridge.submit(&store, "Question".into()),
            "a conflict requires an explicit reload"
        );
    }

    #[test]
    fn stale_loads_do_not_steal_selection_or_revoke_another_stream() {
        let (mut bridge, mut store, id) = loaded();
        let token = store
            .begin_turn("Question".into(), None, "Thinking".into(), true)
            .unwrap()
            .token;
        bridge.pending.insert(
            401,
            Pending::Open {
                selection: bridge.selection,
                keep_local: true,
            },
        );
        bridge.leave();
        let other = id + 1;
        assert!(bridge
            .handle_reply(
                &mut store,
                401,
                Ok(WorkspaceReply::Chat {
                    chat: stored(other, "jterm:api:other", "other draft")
                }),
                false
            )
            .is_empty());
        assert_eq!(store.active_id(), id);
        assert!(store
            .push_delta(token, "still the original response")
            .is_some());
        assert_eq!(store.active_partial(), "still the original response");
    }

    #[test]
    fn explicit_reload_preserves_a_conflicted_local_draft() {
        let (mut bridge, mut store, id) = loaded();
        store.set_active_draft("unsaved local draft".into());
        bridge.changed(&store);
        bridge.known.get_mut(&id).unwrap().blocked = true;
        bridge.pending.insert(
            500,
            Pending::Open {
                selection: bridge.selection,
                keep_local: true,
            },
        );
        bridge.handle_reply(
            &mut store,
            500,
            Ok(WorkspaceReply::Chat {
                chat: stored(id, "jterm:api:original", "remote draft"),
            }),
            false,
        );
        assert_eq!(store.active_draft(), "unsaved local draft");
        assert!(!bridge.known[&id].blocked);
        assert!(bridge.known[&id].dirty);
    }
    #[test]
    fn completion_adopts_remote_draft_before_any_following_save() {
        let (mut bridge, mut store, id) = loaded();
        store.set_active_draft(String::new());
        bridge.pending.insert(700, Pending::Finish { chat_id: id });
        let remote = stored(id, "jterm:api:original", "written in another window");
        let effects = bridge.handle_reply(
            &mut store,
            700,
            Ok(WorkspaceReply::Chat { chat: remote }),
            false,
        );
        assert!(matches!(effects.as_slice(), [Effect::SyncDraft(found)] if *found == id));
        assert_eq!(store.chat_draft(id), Some("written in another window"));
        let outgoing = store.snapshot_chat_for_persistence(id, false).unwrap();
        assert_eq!(
            outgoing.draft(),
            "written in another window",
            "post-completion history/context Save cannot erase the remote draft"
        );
        assert!(!bridge.known[&id].blocked);
    }

    #[test]
    fn completion_blocks_when_both_drafts_diverge() {
        let (mut bridge, mut store, id) = loaded();
        store.set_active_draft("local unsent thought".into());
        bridge.changed(&store);
        bridge.pending.insert(701, Pending::Finish { chat_id: id });
        let remote = stored(id, "jterm:api:original", "remote unsent thought");
        bridge.handle_reply(
            &mut store,
            701,
            Ok(WorkspaceReply::Chat { chat: remote }),
            false,
        );
        assert_eq!(store.chat_draft(id), Some("local unsent thought"));
        assert!(bridge.known[&id].blocked);
        let retained =
            ConversationSnapshot::from_json(&bridge.known[&id].stored.snapshot_json).unwrap();
        assert_eq!(
            retained.active_chat().unwrap().draft(),
            "remote unsent thought"
        );
        assert!(!bridge.submit(&store, "local unsent thought".into()));
    }

    #[test]
    fn stop_before_receipt_cancels_without_dispatching_provider() {
        let (mut bridge, mut store, id) = loaded();
        let request = acknowledge_save(&mut bridge, &mut store, id);
        assert!(bridge.cancel_queued(id));
        let effects = bridge.handle_reply(
            &mut store,
            request,
            Ok(WorkspaceReply::Submission {
                receipt: SubmissionReceipt {
                    id: ConversationId("jterm:api:original".into()),
                    epoch: 1,
                    nonce: "stopped-before-http".into(),
                    draft_revision: 2,
                },
            }),
            false,
        );
        assert!(effects.is_empty());
        assert!(bridge.awaiting_dispatch.is_empty());
        assert_eq!(store.active_draft(), "Question");
        assert!(bridge
            .pending
            .values()
            .any(|pending| matches!(pending, Pending::Finish { chat_id } if *chat_id == id)));
    }

    #[test]
    fn late_widget_event_cannot_edit_a_reopened_conversation() {
        let (mut bridge, store, id) = loaded();
        let old = bridge.reference(id);
        bridge.leave();
        assert!(!bridge.accepts(old, store.active_id()));
    }
}
