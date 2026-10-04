use super::{ChatWidget, ExternalEditorState};
use state_machines::state_machine;

state_machine! {
    name: EditorAdmission,
    dynamic: true,
    initial: Closed,
    states: [Closed, Requested, Active],
    events {
        request { transition: { from: Closed, to: Requested } }
        activate { transition: { from: Requested, to: Active } }
        finish {
            transition: { from: Requested, to: Closed }
            transition: { from: Active, to: Closed }
            transition: { from: Closed, internal: true }
        }
    }
}

pub(super) struct ExternalEditor {
    machine: DynamicEditorAdmission<()>,
}

impl Default for ExternalEditor {
    fn default() -> Self {
        Self {
            machine: EditorAdmission::new(()).into_dynamic(),
        }
    }
}

impl ExternalEditor {
    pub(super) fn state(&self) -> ExternalEditorState {
        match self.machine.current_state() {
            EditorAdmissionState::Closed => ExternalEditorState::Closed,
            EditorAdmissionState::Requested => ExternalEditorState::Requested,
            EditorAdmissionState::Active => ExternalEditorState::Active,
        }
    }

    pub(super) fn request(&mut self) -> bool {
        self.machine.handle(EditorAdmissionEvent::Request).is_ok()
    }

    pub(super) fn activate(&mut self) -> bool {
        self.machine.handle(EditorAdmissionEvent::Activate).is_ok()
    }

    pub(super) fn finish(&mut self) {
        assert!(self.machine.handle(EditorAdmissionEvent::Finish).is_ok());
    }
}

pub(super) struct EditorSession<'a>(&'a mut ChatWidget);

impl<'a> EditorSession<'a> {
    pub(super) fn new(widget: &'a mut ChatWidget) -> Self {
        assert_eq!(widget.external_editor_state(), ExternalEditorState::Active);
        Self(widget)
    }
}

impl Drop for EditorSession<'_> {
    fn drop(&mut self) {
        self.0.finish_external_editor();
        self.0.set_footer_hint_override(None);
        self.0.request_redraw();
    }
}
