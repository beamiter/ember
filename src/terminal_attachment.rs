use jterm_protocol::terminal::Attachment;
use std::sync::Arc;

/// One writable native view on a retained in-process terminal actor.
pub(crate) struct TerminalViewAttachment {
    ticket: Attachment,
    input_owner: Option<Arc<()>>,
}

impl TerminalViewAttachment {
    pub(crate) fn new(runtime_epoch: u32) -> Self {
        Self {
            ticket: Attachment {
                session: uuid::Uuid::new_v4().to_string(),
                epoch: runtime_epoch,
                generation: 1,
            },
            input_owner: Some(Arc::new(())),
        }
    }

    pub(crate) fn ticket(&self) -> Attachment {
        self.ticket.clone()
    }

    pub(crate) fn matches(&self, ticket: &Attachment) -> bool {
        &self.ticket == ticket
    }

    pub(crate) fn attached(&self) -> bool {
        self.input_owner.is_some()
    }

    pub(crate) fn input_owner(&self) -> Option<Arc<()>> {
        self.input_owner.clone()
    }

    pub(crate) fn input_ticket(&self) -> Option<Attachment> {
        self.attached().then(|| self.ticket())
    }

    pub(crate) fn can_switch(&self, ticket: &Attachment, attach: bool) -> bool {
        self.matches(ticket) && self.attached() != attach && self.ticket.generation < u32::MAX
    }

    pub(crate) fn switch(&mut self, ticket: &Attachment, attach: bool) -> bool {
        if !self.can_switch(ticket, attach) {
            return false;
        }
        self.ticket.generation += 1;
        self.input_owner = attach.then(|| Arc::new(()));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::TerminalViewAttachment;
    use std::sync::Arc;

    #[test]
    fn detach_keeps_runtime_identity_and_invalidates_old_view_control() {
        let mut view = TerminalViewAttachment::new(7);
        let other = TerminalViewAttachment::new(8);
        let original = view.input_ticket().unwrap();
        let input_owner = view.input_owner().unwrap();
        assert!(!view.switch(&other.ticket(), false));
        assert!(view.switch(&original, false));
        assert!(view.input_owner().is_none());
        assert!(view.input_ticket().is_none());
        assert!(!view.switch(&original, true));
        let detached = view.ticket();
        assert!(view.switch(&detached, true));
        assert!(!view.switch(&detached, false));
        let resumed = view.input_ticket().unwrap();
        assert_eq!(
            (&resumed.session, resumed.epoch),
            (&original.session, original.epoch)
        );
        assert!(resumed.generation > detached.generation);
        assert!(!Arc::ptr_eq(&input_owner, &view.input_owner().unwrap()));
    }
}
