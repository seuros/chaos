use rand::RngExt;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::sync::Notify;
use tokio::sync::mpsc;
use tokio::sync::watch;
use tokio::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::exec::ExecCommandRequest;
use crate::exec::ExecContext;
use crate::exec::ExecError;
use crate::exec::ExecProcessManager;
use crate::exec::ExecTaskSnapshot;
use crate::exec::MAX_EXEC_PROCESSES;
use crate::exec::MAX_YIELD_TIME_MS;
use crate::exec::MIN_EMPTY_YIELD_TIME_MS;
use crate::exec::MIN_YIELD_TIME_MS;
use crate::exec::ProcessEntry;
use crate::exec::ProcessStore;
use crate::exec::WARNING_EXEC_PROCESSES;
use crate::exec::WriteStdinRequest;
use crate::exec::async_watcher::emit_exec_end;
use crate::exec::async_watcher::spawn_exit_watcher;
use crate::exec::async_watcher::start_streaming_output;
use crate::exec::clamp_yield_time;
use crate::exec::generate_chunk_id;
use crate::exec::head_tail_buffer::HeadTailBuffer;
use crate::exec::output_lifecycle::Collection;
use crate::exec::process::ExecProcess;
use crate::exec::process::OutputBuffer;
use crate::exec::process::OutputHandles;
use crate::exec::process::SpawnLifecycleHandle;
use crate::exec_env::create_env;
use crate::exec_policy::ExecApprovalRequest;
use crate::protocol::ExecCommandSource;
use crate::sandboxing::ExecRequest;
use crate::tools::context::ExecCommandToolOutput;
use crate::tools::events::ToolEmitter;
use crate::tools::events::ToolEventCtx;
use crate::tools::events::ToolEventStage;
use crate::tools::network_approval::DeferredNetworkApproval;
use crate::tools::network_approval::finish_deferred_network_approval;
use crate::tools::orchestrator::ToolOrchestrator;
use crate::tools::runtimes::exec::ExecRequest as ExecToolRequest;
use crate::tools::runtimes::exec::ExecRuntime;
use crate::tools::sandboxing::ToolCtx;
use crate::truncate::approx_token_count;

const EXEC_ENV: [(&str, &str); 10] = [
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
    ("LANG", "C.UTF-8"),
    ("LC_CTYPE", "C.UTF-8"),
    ("LC_ALL", "C.UTF-8"),
    ("COLORTERM", ""),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("GH_PAGER", "cat"),
    ("CHAOS_CI", "1"),
];

/// Test-only override for deterministic exec process IDs.
///
/// In production builds this value should remain at its default (`false`) and
/// must not be toggled.
static FORCE_DETERMINISTIC_PROCESS_IDS: AtomicBool = AtomicBool::new(false);

pub(super) fn set_deterministic_process_ids_for_tests(enabled: bool) {
    FORCE_DETERMINISTIC_PROCESS_IDS.store(enabled, Ordering::Relaxed);
}

fn deterministic_process_ids_forced_for_tests() -> bool {
    FORCE_DETERMINISTIC_PROCESS_IDS.load(Ordering::Relaxed)
}

fn should_use_deterministic_process_ids() -> bool {
    cfg!(test) || deterministic_process_ids_forced_for_tests()
}

fn apply_exec_env(mut env: HashMap<String, String>) -> HashMap<String, String> {
    for (key, value) in EXEC_ENV {
        env.insert(key.to_string(), value.to_string());
    }
    env
}

struct PreparedProcessHandles {
    process: Arc<ExecProcess>,
    writer_tx: mpsc::Sender<Vec<u8>>,
    output_buffer: OutputBuffer,
    output_notify: Arc<Notify>,
    output_closed: Arc<AtomicBool>,
    output_closed_notify: Arc<Notify>,
    cancellation_token: CancellationToken,
    pause_state: Option<watch::Receiver<bool>>,
    call_id: String,
    command: Vec<String>,
    process_id: i32,
    tty: bool,
}

impl ExecProcessManager {
    pub(crate) async fn allocate_process_id(&self) -> i32 {
        loop {
            let mut store = self.process_store.lock().await;

            let process_id = if should_use_deterministic_process_ids() {
                // test or deterministic mode
                store
                    .reserved_process_ids
                    .iter()
                    .copied()
                    .max()
                    .map(|m| std::cmp::max(m, 999) + 1)
                    .unwrap_or(1000)
            } else {
                // production mode → random
                rand::rng().random_range(1_000..100_000)
            };

            if store.reserved_process_ids.contains(&process_id) {
                continue;
            }

            store.reserved_process_ids.insert(process_id);
            return process_id;
        }
    }

    pub(crate) async fn release_process_id(&self, process_id: i32) {
        let removed = {
            let mut store = self.process_store.lock().await;
            store.remove(process_id)
        };
        if let Some(entry) = removed {
            Self::unregister_network_approval_for_entry(&entry).await;
        }
    }

    async fn unregister_network_approval_for_entry(entry: &ProcessEntry) {
        if let Some(network_approval_id) = entry.network_approval_id.as_deref()
            && let Some(session) = entry.session.upgrade()
        {
            session
                .services
                .network_approval
                .unregister_call(network_approval_id)
                .await;
        }
    }

    pub(crate) async fn exec_command(
        &self,
        request: ExecCommandRequest,
        context: &ExecContext,
    ) -> Result<ExecCommandToolOutput, ExecError> {
        let cwd = request
            .workdir
            .clone()
            .unwrap_or_else(|| context.turn.cwd.clone());
        let process = self
            .open_session_with_sandbox(&request, cwd.clone(), context)
            .await;

        let (process, mut deferred_network_approval) = match process {
            Ok((process, deferred_network_approval)) => {
                (Arc::new(process), deferred_network_approval)
            }
            Err(err) => {
                self.release_process_id(request.process_id).await;
                return Err(err);
            }
        };

        let transcript = Arc::new(tokio::sync::Mutex::new(HeadTailBuffer::default()));
        let event_ctx = ToolEventCtx::new(
            context.session.as_ref(),
            context.turn.as_ref(),
            &context.call_id,
            /*turn_diff_tracker*/ None,
        );
        let emitter = ToolEmitter::exec(
            &request.command,
            cwd.clone(),
            ExecCommandSource::ExecStartup,
            Some(request.process_id.to_string()),
        );
        emitter.emit(event_ctx, ToolEventStage::Begin).await;

        start_streaming_output(&process, context, Arc::clone(&transcript));
        let start = Instant::now();
        // Persist live sessions before the initial yield wait so interrupting the
        // turn cannot drop the last Arc and terminate the background process.
        let process_started_alive = !process.has_exited() && process.exit_code().is_none();
        if process_started_alive {
            let network_approval_id = deferred_network_approval
                .as_ref()
                .map(|deferred| deferred.registration_id().to_string());
            self.store_process(
                Arc::clone(&process),
                context,
                &request.command,
                cwd.clone(),
                start,
                request.process_id,
                request.tty,
                network_approval_id,
                Arc::clone(&transcript),
            )
            .await;
        }

        let yield_time_ms = clamp_yield_time(request.yield_time_ms);
        // For the initial exec_command call, we both stream output to events
        // (via start_streaming_output above) and collect a snapshot here for
        // the tool response body.
        let OutputHandles {
            output_buffer,
            output_notify,
            output_closed,
            output_closed_notify,
            cancellation_token,
        } = process.output_handles();
        let deadline = start + Duration::from_millis(yield_time_ms);
        let collected = Self::collect_output_until_deadline(
            &output_buffer,
            &output_notify,
            &output_closed,
            &output_closed_notify,
            &cancellation_token,
            Some(
                context
                    .session
                    .subscribe_out_of_band_elicitation_pause_state(),
            ),
            deadline,
        )
        .await;
        let wall_time = Instant::now().saturating_duration_since(start);

        let text = String::from_utf8_lossy(&collected).to_string();
        let chunk_id = generate_chunk_id();
        let process_id = request.process_id;
        let (response_process_id, exit_code) = if process_started_alive {
            match self.refresh_process_state(process_id).await {
                ProcessStatus::Alive {
                    exit_code,
                    process_id,
                    ..
                } => (Some(process_id), exit_code),
                ProcessStatus::Exited { exit_code, .. } => {
                    process.check_for_sandbox_denial_with_text(&text).await?;
                    (None, exit_code)
                }
                ProcessStatus::Unknown => {
                    return Err(ExecError::UnknownProcessId { process_id });
                }
            }
        } else {
            // Short‑lived command: emit ExecCommandEnd immediately using the
            // same helper as the background watcher, so all end events share
            // one implementation.
            let exit_code = process.exit_code();
            let exit = exit_code.unwrap_or(-1);
            emit_exec_end(
                Arc::clone(&context.session),
                Arc::clone(&context.turn),
                context.call_id.clone(),
                request.command.clone(),
                cwd.clone(),
                Some(process_id.to_string()),
                Arc::clone(&transcript),
                text.clone(),
                exit,
                wall_time,
            )
            .await;

            self.release_process_id(request.process_id).await;
            finish_deferred_network_approval(
                context.session.as_ref(),
                deferred_network_approval.take(),
            )
            .await;
            process.check_for_sandbox_denial_with_text(&text).await?;
            (None, exit_code)
        };

        let original_token_count = approx_token_count(&text);
        let response = ExecCommandToolOutput {
            event_call_id: context.call_id.clone(),
            chunk_id,
            wall_time,
            raw_output: collected,
            max_output_tokens: request.max_output_tokens,
            process_id: response_process_id,
            exit_code,
            original_token_count: Some(original_token_count),
            session_command: Some(request.command.clone()),
            task_id: None,
        };

        Ok(response)
    }

    pub(crate) async fn write_stdin(
        &self,
        request: WriteStdinRequest<'_>,
    ) -> Result<ExecCommandToolOutput, ExecError> {
        let process_id = request.process_id;

        let PreparedProcessHandles {
            process,
            writer_tx,
            output_buffer,
            output_notify,
            output_closed,
            output_closed_notify,
            cancellation_token,
            pause_state,
            call_id,
            command: session_command,
            process_id,
            tty,
            ..
        } = self.prepare_process_handles(process_id).await?;

        if !request.input.is_empty() {
            if !tty {
                return Err(ExecError::StdinClosed);
            }
            Self::send_input(&writer_tx, request.input.as_bytes()).await?;
            // Give the remote process a brief window to react so that we are
            // more likely to capture its output in the poll below.
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let yield_time_ms = {
            // Empty polls use configurable background timeout bounds. Non-empty
            // writes keep a fixed max cap so interactive stdin remains responsive.
            let time_ms = request.yield_time_ms.max(MIN_YIELD_TIME_MS);
            if request.input.is_empty() {
                time_ms.clamp(MIN_EMPTY_YIELD_TIME_MS, self.max_write_stdin_yield_time_ms)
            } else {
                time_ms.min(MAX_YIELD_TIME_MS)
            }
        };
        let start = Instant::now();
        let deadline = start + Duration::from_millis(yield_time_ms);
        let collected = Self::collect_output_until_deadline(
            &output_buffer,
            &output_notify,
            &output_closed,
            &output_closed_notify,
            &cancellation_token,
            pause_state,
            deadline,
        )
        .await;
        let wall_time = Instant::now().saturating_duration_since(start);

        let text = String::from_utf8_lossy(&collected).to_string();
        let original_token_count = approx_token_count(&text);
        let chunk_id = generate_chunk_id();

        // After polling, refresh_process_state tells us whether the PTY is
        // still alive or has exited and been removed from the store; we thread
        // that through so the handler can tag TerminalInteraction with an
        // appropriate process_id and exit_code.
        let status = self.refresh_process_state(process_id).await;
        let (process_id, exit_code, event_call_id) = match status {
            ProcessStatus::Alive {
                exit_code,
                call_id,
                process_id,
            } => (Some(process_id), exit_code, call_id),
            ProcessStatus::Exited { exit_code, entry } => {
                let call_id = entry.call_id.clone();
                (None, exit_code, call_id)
            }
            ProcessStatus::Unknown => {
                if process.has_exited() {
                    (None, process.exit_code(), call_id)
                } else {
                    return Err(ExecError::UnknownProcessId {
                        process_id: request.process_id,
                    });
                }
            }
        };

        let response = ExecCommandToolOutput {
            event_call_id,
            chunk_id,
            wall_time,
            raw_output: collected,
            max_output_tokens: request.max_output_tokens,
            process_id,
            exit_code,
            original_token_count: Some(original_token_count),
            session_command: Some(session_command.clone()),
            task_id: None,
        };

        Ok(response)
    }

    pub(crate) async fn terminate_process(&self, process_id: i32) -> Result<(), ExecError> {
        let removed = {
            let mut store = self.process_store.lock().await;
            store
                .remove(process_id)
                .ok_or(ExecError::UnknownProcessId { process_id })?
        };
        Self::unregister_network_approval_for_entry(&removed).await;
        removed.process.terminate();
        Ok(())
    }

    pub(crate) async fn subscribe_completion(
        &self,
        process_id: i32,
    ) -> Result<watch::Receiver<ExecTaskSnapshot>, ExecError> {
        self.process_store
            .lock()
            .await
            .processes
            .get(&process_id)
            .map(|entry| entry.completion.clone())
            .ok_or(ExecError::UnknownProcessId { process_id })
    }

    async fn refresh_process_state(&self, process_id: i32) -> ProcessStatus {
        let status = {
            let mut store = self.process_store.lock().await;
            let Some(entry) = store.processes.get(&process_id) else {
                return ProcessStatus::Unknown;
            };

            let exit_code = entry.process.exit_code();
            let process_id = entry.process_id;

            if entry.process.has_exited() {
                let Some(entry) = store.remove(process_id) else {
                    return ProcessStatus::Unknown;
                };
                ProcessStatus::Exited {
                    exit_code,
                    entry: Box::new(entry),
                }
            } else {
                ProcessStatus::Alive {
                    exit_code,
                    call_id: entry.call_id.clone(),
                    process_id,
                }
            }
        };
        if let ProcessStatus::Exited { entry, .. } = &status {
            Self::unregister_network_approval_for_entry(entry).await;
        }
        status
    }

    async fn prepare_process_handles(
        &self,
        process_id: i32,
    ) -> Result<PreparedProcessHandles, ExecError> {
        let mut store = self.process_store.lock().await;
        let entry = store
            .processes
            .get_mut(&process_id)
            .ok_or(ExecError::UnknownProcessId { process_id })?;
        entry.last_used = Instant::now();
        let OutputHandles {
            output_buffer,
            output_notify,
            output_closed,
            output_closed_notify,
            cancellation_token,
        } = entry.process.output_handles();
        let pause_state = entry
            .session
            .upgrade()
            .map(|session| session.subscribe_out_of_band_elicitation_pause_state());

        Ok(PreparedProcessHandles {
            process: Arc::clone(&entry.process),
            writer_tx: entry.process.writer_sender(),
            output_buffer,
            output_notify,
            output_closed,
            output_closed_notify,
            cancellation_token,
            pause_state,
            call_id: entry.call_id.clone(),
            command: entry.command.clone(),
            process_id: entry.process_id,
            tty: entry.tty,
        })
    }

    async fn send_input(writer_tx: &mpsc::Sender<Vec<u8>>, data: &[u8]) -> Result<(), ExecError> {
        writer_tx
            .send(data.to_vec())
            .await
            .map_err(|_| ExecError::WriteToStdin)
    }

    #[allow(clippy::too_many_arguments)]
    async fn store_process(
        &self,
        process: Arc<ExecProcess>,
        context: &ExecContext,
        command: &[String],
        cwd: PathBuf,
        started_at: Instant,
        process_id: i32,
        tty: bool,
        network_approval_id: Option<String>,
        transcript: Arc<tokio::sync::Mutex<HeadTailBuffer>>,
    ) {
        let (completion_tx, completion) = watch::channel(ExecTaskSnapshot::Running);
        let entry = ProcessEntry {
            process: Arc::clone(&process),
            call_id: context.call_id.clone(),
            process_id,
            command: command.to_vec(),
            tty,
            network_approval_id,
            session: Arc::downgrade(&context.session),
            last_used: started_at,
            completion,
        };
        let (number_processes, pruned_entry) = {
            let mut store = self.process_store.lock().await;
            let pruned_entry = Self::prune_processes_if_needed(&mut store);
            store.processes.insert(process_id, entry);
            (store.processes.len(), pruned_entry)
        };
        // prune_processes_if_needed runs while holding process_store; do async
        // network-approval cleanup only after dropping that lock.
        if let Some(pruned_entry) = pruned_entry {
            Self::unregister_network_approval_for_entry(&pruned_entry).await;
            pruned_entry.process.terminate();
        }

        if number_processes >= WARNING_EXEC_PROCESSES {
            context
                .session
                .record_model_warning(
                    format!("The maximum number of exec processes you can keep open is {WARNING_EXEC_PROCESSES} and you currently have {number_processes} processes open. Reuse older processes or close them to prevent automatic pruning of old processes"),
                    &context.turn
                )
                .await;
        };

        spawn_exit_watcher(
            Arc::clone(&process),
            Arc::clone(&context.session),
            Arc::clone(&context.turn),
            context.call_id.clone(),
            command.to_vec(),
            cwd,
            process_id,
            transcript,
            started_at,
            completion_tx,
        );
    }

    pub(crate) async fn open_session_with_exec_env(
        &self,
        env: &ExecRequest,
        tty: bool,
        mut spawn_lifecycle: SpawnLifecycleHandle,
    ) -> Result<ExecProcess, ExecError> {
        let (program, args) = env
            .command
            .split_first()
            .ok_or(ExecError::MissingCommandLine)?;
        let inherited_fds = spawn_lifecycle.inherited_fds();

        let spawn_result = if tty {
            chaos_pty::pty::spawn_process_with_inherited_fds(
                program,
                args,
                env.cwd.as_path(),
                &env.env,
                &env.arg0,
                chaos_pty::TerminalSize::default(),
                &inherited_fds,
            )
            .await
        } else {
            chaos_pty::pipe::spawn_process_no_stdin_with_inherited_fds(
                program,
                args,
                env.cwd.as_path(),
                &env.env,
                &env.arg0,
                &inherited_fds,
            )
            .await
        };
        let spawned = spawn_result.map_err(|err| ExecError::create_process(err.to_string()))?;
        spawn_lifecycle.after_spawn();
        ExecProcess::from_spawned(spawned, env.sandbox, spawn_lifecycle).await
    }

    pub(super) async fn open_session_with_sandbox(
        &self,
        request: &ExecCommandRequest,
        cwd: PathBuf,
        context: &ExecContext,
    ) -> Result<(ExecProcess, Option<DeferredNetworkApproval>), ExecError> {
        let env = apply_exec_env(create_env(
            &context.turn.shell_environment_policy,
            Some(context.session.conversation_id),
        ));
        let mut orchestrator = ToolOrchestrator::new();
        let mut runtime = ExecRuntime::new(self);
        let permission_snapshot = context.session.permission_snapshot(&context.turn).await;
        let exec_approval_requirement = context
            .session
            .services
            .exec_policy
            .create_exec_approval_requirement_for_command_in(
                ExecApprovalRequest {
                    command: &request.command,
                    approval_policy: permission_snapshot.approval_policy,
                    vfs_policy: &permission_snapshot.vfs_policy,
                    sandbox_permissions: if request.additional_permissions_preapproved {
                        crate::sandboxing::SandboxPermissions::UseDefault
                    } else {
                        request.sandbox_permissions
                    },
                    prefix_rule: request.prefix_rule.clone(),
                },
                &cwd,
            )
            .await;
        let req = ExecToolRequest {
            command: request.command.clone(),
            cwd,
            env,
            explicit_env_overrides: apply_exec_env(
                context.turn.shell_environment_policy.r#set.clone(),
            ),
            network: request.network.clone(),
            tty: request.tty,
            sandbox_permissions: request.sandbox_permissions,
            additional_permissions: request.additional_permissions.clone(),
            justification: request.justification.clone(),
            exec_approval_requirement,
        };
        let tool_ctx = ToolCtx {
            session: context.session.clone(),
            turn: context.turn.clone(),
            call_id: context.call_id.clone(),
            tool_name: "exec_command".to_string(),
        };
        orchestrator
            .run(&mut runtime, &req, &tool_ctx, &context.turn)
            .await
            .map(|result| (result.output, result.deferred_network_approval))
            .map_err(|e| ExecError::create_process(format!("{e:?}")))
    }

    pub(super) async fn collect_output_until_deadline(
        output_buffer: &OutputBuffer,
        output_notify: &Arc<Notify>,
        output_closed: &Arc<AtomicBool>,
        output_closed_notify: &Arc<Notify>,
        cancellation_token: &CancellationToken,
        pause_state: Option<watch::Receiver<bool>>,
        deadline: Instant,
    ) -> Vec<u8> {
        let mut collection = Collection::new(deadline, pause_state);
        collection.observe_exit(cancellation_token.is_cancelled());
        loop {
            collection.wait_while_paused().await;
            let notified = output_notify.notified();
            let closed = output_closed_notify.notified();
            tokio::pin!(notified);
            tokio::pin!(closed);
            notified.as_mut().enable();
            closed.as_mut().enable();
            let drained_chunks: Vec<Vec<u8>>;
            {
                let mut guard = output_buffer.lock().await;
                drained_chunks = guard.drain_chunks();
            }

            if drained_chunks.is_empty() {
                collection.observe_exit(cancellation_token.is_cancelled());
                if collection.exited() && output_closed.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                let remaining = collection
                    .deadline()
                    .saturating_duration_since(Instant::now());
                if remaining == Duration::ZERO {
                    break;
                }

                let pause_state = collection.pause_receiver();
                if collection.exited() {
                    let now = Instant::now();
                    let close_wait_deadline = collection.close_deadline(now);
                    let close_wait_remaining = close_wait_deadline.saturating_duration_since(now);
                    if close_wait_remaining == Duration::ZERO {
                        break;
                    }
                    tokio::select! {
                        _ = &mut notified => {}
                        _ = &mut closed => {}
                        _ = tokio::time::sleep(close_wait_remaining) => break,
                        _ = Self::wait_for_pause_change(pause_state.as_ref()) => {}
                    }
                    continue;
                }

                let exit_notified = cancellation_token.cancelled();
                tokio::pin!(exit_notified);
                tokio::select! {
                    _ = &mut notified => {}
                    _ = &mut exit_notified => collection.observe_exit(true),
                    _ = tokio::time::sleep(remaining) => break,
                    _ = Self::wait_for_pause_change(pause_state.as_ref()) => {}
                }
                continue;
            }

            collection.append(drained_chunks);

            collection.observe_exit(cancellation_token.is_cancelled());
            if Instant::now() >= collection.deadline() {
                break;
            }
        }

        collection.finish()
    }

    async fn wait_for_pause_change(pause_state: Option<&watch::Receiver<bool>>) {
        match pause_state {
            Some(pause_state) => {
                let mut receiver = pause_state.clone();
                let _ = receiver.changed().await;
            }
            None => std::future::pending::<()>().await,
        }
    }

    fn prune_processes_if_needed(store: &mut ProcessStore) -> Option<ProcessEntry> {
        if store.processes.len() < MAX_EXEC_PROCESSES {
            return None;
        }

        let meta: Vec<(i32, Instant, bool)> = store
            .processes
            .iter()
            .map(|(id, entry)| (*id, entry.last_used, entry.process.has_exited()))
            .collect();

        if let Some(process_id) = Self::process_id_to_prune_from_meta(&meta) {
            return store.remove(process_id);
        }

        None
    }

    // Centralized pruning policy so we can easily swap strategies later.
    fn process_id_to_prune_from_meta(meta: &[(i32, Instant, bool)]) -> Option<i32> {
        if meta.is_empty() {
            return None;
        }

        let mut by_recency = meta.to_vec();
        by_recency.sort_by_key(|(_, last_used, _)| Reverse(*last_used));
        let protected: HashSet<i32> = by_recency
            .iter()
            .take(8)
            .map(|(process_id, _, _)| *process_id)
            .collect();

        let mut lru = meta.to_vec();
        lru.sort_by_key(|(_, last_used, _)| *last_used);

        if let Some((process_id, _, _)) = lru
            .iter()
            .find(|(process_id, _, exited)| !protected.contains(process_id) && *exited)
        {
            return Some(*process_id);
        }

        lru.into_iter()
            .find(|(process_id, _, _)| !protected.contains(process_id))
            .map(|(process_id, _, _)| process_id)
    }

    pub(crate) async fn terminate_all_processes(&self) {
        let entries: Vec<ProcessEntry> = {
            let mut processes = self.process_store.lock().await;
            let entries: Vec<ProcessEntry> = processes
                .processes
                .drain()
                .map(|(_, entry)| entry)
                .collect();
            processes.reserved_process_ids.clear();
            entries
        };

        for entry in entries {
            Self::unregister_network_approval_for_entry(&entry).await;
            entry.process.terminate();
        }
    }
}

enum ProcessStatus {
    Alive {
        exit_code: Option<i32>,
        call_id: String,
        process_id: i32,
    },
    Exited {
        exit_code: Option<i32>,
        entry: Box<ProcessEntry>,
    },
    Unknown,
}

#[cfg(test)]
#[path = "process_manager_tests.rs"]
mod tests;
