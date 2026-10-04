use state_machines::state_machine;
use tempfile::TempPath;
use tokio::process::Child;

pub(super) struct EditorResources {
    pub path: TempPath,
    pub child: Child,
}

pub(super) struct EditorProcess(Option<EditorResources>);

impl std::fmt::Debug for EditorProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EditorProcess(..)")
    }
}

impl EditorProcess {
    pub(super) fn new(resources: EditorResources) -> Self {
        Self(Some(resources))
    }

    pub(super) fn resources_mut(&mut self) -> &mut EditorResources {
        self.0
            .as_mut()
            .unwrap_or_else(|| unreachable!("editor owns process resources"))
    }
}

impl Drop for EditorProcess {
    fn drop(&mut self) {
        let Some(mut resources) = self.0.take() else {
            return;
        };
        if matches!(resources.child.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = resources.child.start_kill();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = resources.child.wait().await;
                drop(resources);
            });
        }
    }
}

state_machine! {
    name: EditorInvocation,
    dynamic: true,
    initial: Preparing,
    states: [Preparing, Running(EditorProcess), Completed],
    final_states: [Completed],
    events {
        launch {
            payload: Option<EditorProcess>,
            transition: { from: Preparing, to: Running, data: own_process }
        }
        finish { transition: { from: Running, to: Completed } }
    }
}

impl<C, S> EditorInvocation<C, S> {
    fn own_process(&self, process: &mut Option<EditorProcess>) -> EditorProcess {
        process
            .take()
            .unwrap_or_else(|| unreachable!("launch owns editor process"))
    }
}
