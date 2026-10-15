//! Live containment is independent of foreground status and accounting receipts.
use super::*;

type PendingAdmissions = Arc<std::sync::Mutex<HashMap<uuid::Uuid, ProcessId>>>;

/// Dropping an unfinished boot cannot prove that its startup resources stopped.
/// Keep the entry quarantined; only successful registry handoff clears it.
pub(crate) struct PendingAdmission {
    id: uuid::Uuid,
    parent: ProcessId,
    entries: PendingAdmissions,
    epoch: Arc<crate::invocation::AdmissionEpoch>,
}

impl PendingAdmission {
    pub(super) fn parent(&self) -> ProcessId {
        self.parent
    }

    pub(crate) fn registered(self) -> ChaosResult<()> {
        let _mutation = self.epoch.mutate()?;
        self.entries
            .lock()
            .map_err(|_| ChaosErr::Fatal("descendant admission state unavailable".into()))?
            .remove(&self.id);
        Ok(())
    }
}

impl ProcessTableState {
    pub(crate) fn pending_admission(&self, parent: ProcessId) -> ChaosResult<PendingAdmission> {
        let _mutation = self.admission_epoch.mutate()?;
        let id = uuid::Uuid::new_v4();
        self.pending_descendants
            .lock()
            .map_err(|_| ChaosErr::Fatal("descendant admission state unavailable".into()))?
            .insert(id, parent);
        Ok(PendingAdmission {
            id,
            parent,
            entries: self.pending_descendants.clone(),
            epoch: self.admission_epoch.clone(),
        })
    }

    pub(super) fn track_descendant(&self, parent: ProcessId, child: ProcessId) -> ChaosResult<()> {
        let _mutation = self.admission_epoch.mutate()?;
        let mut parents = self
            .descendant_parents
            .lock()
            .map_err(|_| ChaosErr::Fatal("descendant lineage unavailable".into()))?;
        if parent == child
            || parents
                .get(&child)
                .is_some_and(|recorded| *recorded != parent)
        {
            return Err(ChaosErr::Fatal("descendant lineage conflict".into()));
        }
        parents.insert(child, parent);
        Ok(())
    }

    pub(crate) async fn register_inline_delegate(
        &self,
        parent: ProcessId,
        chaos: &Chaos,
    ) -> ChaosResult<()> {
        let config = chaos.session.get_config().await;
        chaos
            .session
            .services
            .model_client
            .bind_admission_epoch(self.admission_epoch.clone())?;
        let process_id = chaos.session.conversation_id;
        let process = Arc::new(Process::new(
            Chaos {
                tracks_submissions: chaos.tracks_submissions,
                tx_sub: chaos.tx_sub.clone(),
                rx_event: chaos.rx_event.clone(),
                agent_status: chaos.agent_status.clone(),
                session: chaos.session.clone(),
                session_loop_termination: chaos.session_loop_termination.clone(),
            },
            self.file_watcher.register_config(&config),
        ));
        let mut processes = self.processes.write().await;
        if processes.contains_key(&process_id) {
            return Err(ChaosErr::Fatal(
                "descendant process already registered".into(),
            ));
        }
        self.track_descendant(parent, process_id)?;
        processes.insert(process_id, process);
        Ok(())
    }

    fn include_known_descendants(&self, owned: &mut HashSet<ProcessId>) -> ChaosResult<()> {
        let parents = self
            .descendant_parents
            .lock()
            .map_err(|_| ChaosErr::Fatal("descendant lineage unavailable".into()))?;
        loop {
            let before = owned.len();
            for (child, parent) in parents.iter() {
                if owned.contains(parent) {
                    owned.insert(*child);
                }
            }
            if before == owned.len() {
                return Ok(());
            }
        }
    }
}

impl ProcessTable {
    async fn process_tree(&self, root: ProcessId) -> ChaosResult<Vec<(ProcessId, Arc<Process>)>> {
        let processes: Vec<_> = self
            .state
            .processes
            .read()
            .await
            .iter()
            .map(|(id, process)| (*id, process.clone()))
            .collect();
        let mut edges = Vec::with_capacity(processes.len());
        for (id, process) in processes {
            let source = process.config_snapshot().await.session_source;
            let parent = match source {
                SessionSource::SubAgent(chaos_ipc::protocol::SubAgentSource::ProcessSpawn {
                    parent_process_id,
                    ..
                }) => Some(parent_process_id),
                _ => None,
            };
            edges.push((id, parent, process));
        }
        let mut owned = HashSet::from([root]);
        loop {
            let before = owned.len();
            self.state.include_known_descendants(&mut owned)?;
            for (id, parent, _) in &edges {
                if parent.is_some_and(|parent| owned.contains(&parent)) {
                    owned.insert(*id);
                }
            }
            if owned.len() == before {
                break;
            }
        }
        Ok(edges
            .into_iter()
            .filter(|(id, _, _)| owned.contains(id))
            .map(|(id, _, process)| (id, process))
            .collect())
    }

    fn tree_has_pending_admissions(
        &self,
        root: ProcessId,
        tree: &[(ProcessId, Arc<Process>)],
    ) -> ChaosResult<bool> {
        let mut owned: HashSet<_> = std::iter::once(root)
            .chain(tree.iter().map(|(id, _)| *id))
            .collect();
        self.state.include_known_descendants(&mut owned)?;
        Ok(self
            .state
            .pending_descendants
            .lock()
            .map_err(|_| ChaosErr::Fatal("descendant admission state unavailable".into()))?
            .values()
            .any(|parent| owned.contains(parent)))
    }

    /// A parent's final status is not proof that its delegated tools or nested
    /// agents have stopped. Include pending boots, even before a child PID is
    /// visible, and check live descendants with root admissions lease-fenced.
    pub async fn process_tree_is_quiescent(&self, root: ProcessId) -> ChaosResult<bool> {
        let Some(epoch) = self.state.admission_epoch.snapshot() else {
            return Ok(false);
        };
        let tree = self.process_tree(root).await?;
        if !tree.iter().any(|(id, _)| *id == root) {
            return Err(ChaosErr::ProcessNotFound(root));
        }
        let idle = tree.iter().all(|(_, process)| {
            process.subscribe_activity().borrow().is_quiescent()
                && !process
                    .chaos
                    .session
                    .services
                    .model_client
                    .has_pending_submissions()
        });
        Ok(idle
            && !self.tree_has_pending_admissions(root, &tree)?
            && Some(epoch) == self.state.admission_epoch.snapshot())
    }

    /// The root loop must already be stopped. Retain all nodes until the whole
    /// tree is contained, including late grandchildren. Cancellation, failed or
    /// abandoned boot, and errors never silently discard execution ownership.
    pub async fn shutdown_descendants_and_wait(&self, root: ProcessId) -> ChaosResult<()> {
        let root_process = self.get_process(root).await?;
        if root_process
            .chaos
            .session_loop_termination
            .clone()
            .now_or_never()
            .is_none()
        {
            return Err(ChaosErr::Fatal(
                "root must stop before descendant containment".into(),
            ));
        }
        let mut stopped = HashSet::new();
        for _ in 0..=chaos_journald::MAX_EXECUTION_INVOCATIONS {
            let epoch = self.state.admission_epoch.snapshot();
            let tree = self.process_tree(root).await?;
            if self.tree_has_pending_admissions(root, &tree)? {
                return Err(ChaosErr::Fatal(
                    "pending descendant startup requires independent containment".into(),
                ));
            }
            let pending: Vec<_> = tree
                .into_iter()
                .filter(|(id, _)| *id != root && !stopped.contains(id))
                .collect();
            if pending.is_empty() {
                if epoch.is_none() || epoch != self.state.admission_epoch.snapshot() {
                    continue;
                }
                for id in stopped {
                    self.remove_process(&id).await;
                }
                return Ok(());
            }
            let mut shutdowns = FuturesUnordered::new();
            for (id, process) in pending {
                shutdowns.push(async move { (id, process.shutdown_and_wait().await) });
            }
            while let Some((id, result)) = shutdowns.next().await {
                result?;
                stopped.insert(id);
            }
        }
        Err(ChaosErr::Fatal(
            "descendant containment did not converge".into(),
        ))
    }
}
