use state_machines::state_machine;
use tokio_util::sync::CancellationToken;

use super::state::PendingLoad;

#[derive(Debug)]
pub(super) struct PendingFetch {
    request: PendingLoad,
    cancellation: CancellationToken,
}

impl Drop for PendingFetch {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

state_machine! {
    name: PageFetch,
    dynamic: true,
    initial: LoadIdle,
    states: [LoadIdle, LoadPending(PendingFetch)],
    events {
        begin {
            payload: Option<PendingFetch>,
            transition: { from: LoadIdle, to: LoadPending, data: own_fetch }
        }
        clear {
            transition: { from: LoadPending, to: LoadIdle }
            transition: { from: LoadIdle, internal: true }
        }
    }
}

impl<C, S> PageFetch<C, S> {
    fn own_fetch(&self, fetch: &mut Option<PendingFetch>) -> PendingFetch {
        fetch
            .take()
            .unwrap_or_else(|| unreachable!("pending owns fetch"))
    }
}

pub(super) struct LoadingState {
    machine: DynamicPageFetch<()>,
}

impl Default for LoadingState {
    fn default() -> Self {
        Self {
            machine: PageFetch::new(()).into_dynamic(),
        }
    }
}

impl LoadingState {
    pub(super) fn is_pending(&self) -> bool {
        self.machine.load_pending_data().is_some()
    }

    pub(super) fn begin(&mut self, request: PendingLoad) -> CancellationToken {
        let cancellation = CancellationToken::new();
        assert!(
            self.machine
                .handle(PageFetchEvent::Begin(Some(PendingFetch {
                    request,
                    cancellation: cancellation.clone(),
                })))
                .is_ok()
        );
        cancellation
    }

    pub(super) fn complete(&mut self, request_token: usize) -> Option<PendingLoad> {
        let request = self.machine.load_pending_data()?.request;
        if request.request_token != request_token {
            return None;
        }
        self.clear();
        Some(request)
    }

    pub(super) fn clear(&mut self) {
        assert!(self.machine.handle(PageFetchEvent::Clear).is_ok());
    }
}

#[derive(Debug)]
pub(super) struct SearchToken(usize);

state_machine! {
    name: PickerSearch,
    dynamic: true,
    initial: SearchIdle,
    states: [SearchIdle, Searching(SearchToken)],
    events {
        start {
            payload: usize,
            transition: { from: SearchIdle, to: Searching, data: own_token }
            transition: { from: Searching, to: Searching, data: own_token }
        }
        clear {
            transition: { from: Searching, to: SearchIdle }
            transition: { from: SearchIdle, internal: true }
        }
    }
}

impl<C, S> PickerSearch<C, S> {
    fn own_token(&self, token: &mut usize) -> SearchToken {
        SearchToken(*token)
    }
}

pub(super) struct SearchState {
    machine: DynamicPickerSearch<()>,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            machine: PickerSearch::new(()).into_dynamic(),
        }
    }
}

impl SearchState {
    pub(super) fn active_token(&self) -> Option<usize> {
        self.machine.searching_data().map(|token| token.0)
    }

    pub(super) fn is_active(&self) -> bool {
        self.active_token().is_some()
    }

    pub(super) fn start(&mut self, token: usize) {
        assert!(self.machine.handle(PickerSearchEvent::Start(token)).is_ok());
    }

    pub(super) fn clear(&mut self) {
        assert!(self.machine.handle(PickerSearchEvent::Clear).is_ok());
    }
}
