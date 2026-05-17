//! Review workflow popup methods: branch picker, commit picker, custom prompt,
//! and minion reviewer picker.
use super::super::*;
use chaos_ipc::protocol::ReviewRequest;
use std::path::Path;

impl ChatWidget {
    /// Open the review target picker.
    ///
    /// When `use_reviewer` is true, each target path chains through the minion
    /// picker before firing the review op.
    pub fn open_review_popup(&mut self, use_reviewer: bool) {
        let mut items: Vec<SelectionItem> = Vec::new();

        let reviewer_label = if use_reviewer {
            "[x] With a special reviewer"
        } else {
            "[ ] With a special reviewer"
        };

        items.push(SelectionItem {
            name: reviewer_label.to_string(),
            description: Some("Pick a minion persona to perform the review".into()),
            is_current: use_reviewer,
            actions: vec![Box::new(move |tx: &AppEventSender| {
                tx.send(AppEvent::OpenReviewPopup {
                    use_reviewer: !use_reviewer,
                });
            })],
            // dismiss_on_select: true so the current popup closes before
            // OpenReviewPopup replaces it — prevents view stack growth.
            dismiss_on_select: true,
            ..Default::default()
        });

        items.push(SelectionItem {
            name: "Review uncommitted changes".to_string(),
            actions: vec![build_review_action(
                use_reviewer,
                ReviewRequest {
                    target: ReviewTarget::UncommittedChanges,
                    user_facing_hint: None,
                    reviewer: None,
                },
            )],
            dismiss_on_select: true,
            ..Default::default()
        });

        items.push(SelectionItem {
            name: "Review against a base branch".to_string(),
            description: Some("(PR Style)".into()),
            actions: vec![Box::new({
                let cwd = self.config.cwd.clone();
                move |tx| {
                    tx.send(AppEvent::OpenReviewBranchPicker {
                        cwd: cwd.clone(),
                        use_reviewer,
                    });
                }
            })],
            dismiss_on_select: false,
            ..Default::default()
        });

        items.push(SelectionItem {
            name: "Review a commit".to_string(),
            actions: vec![Box::new({
                let cwd = self.config.cwd.clone();
                move |tx| {
                    tx.send(AppEvent::OpenReviewCommitPicker {
                        cwd: cwd.clone(),
                        use_reviewer,
                    });
                }
            })],
            dismiss_on_select: false,
            ..Default::default()
        });

        items.push(SelectionItem {
            name: "Custom review instructions".to_string(),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenReviewCustomPrompt { use_reviewer });
            })],
            dismiss_on_select: false,
            ..Default::default()
        });

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select a review preset".into()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        });
    }

    pub async fn show_review_branch_picker(&mut self, cwd: &Path, use_reviewer: bool) {
        let branches = local_git_branches(cwd).await;
        let current_branch = current_branch_name(cwd)
            .await
            .unwrap_or_else(|| "(detached HEAD)".to_string());
        let mut items: Vec<SelectionItem> = Vec::with_capacity(branches.len());

        for option in branches {
            let branch = option.clone();
            items.push(SelectionItem {
                name: format!("{current_branch} -> {branch}"),
                actions: vec![build_review_action(
                    use_reviewer,
                    ReviewRequest {
                        target: ReviewTarget::BaseBranch {
                            branch: branch.clone(),
                        },
                        user_facing_hint: None,
                        reviewer: None,
                    },
                )],
                dismiss_on_select: true,
                search_value: Some(option),
                ..Default::default()
            });
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select a base branch".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search branches".to_string()),
            ..Default::default()
        });
    }

    pub async fn show_review_commit_picker(&mut self, cwd: &Path, use_reviewer: bool) {
        let commits = chaos_kern::git_info::recent_commits(cwd, 100).await;

        let mut items: Vec<SelectionItem> = Vec::with_capacity(commits.len());
        for entry in commits {
            let subject = entry.subject.clone();
            let sha = entry.sha.clone();
            let search_val = format!("{subject} {sha}");

            items.push(SelectionItem {
                name: subject.clone(),
                actions: vec![build_review_action(
                    use_reviewer,
                    ReviewRequest {
                        target: ReviewTarget::Commit {
                            sha: sha.clone(),
                            title: Some(subject.clone()),
                        },
                        user_facing_hint: None,
                        reviewer: None,
                    },
                )],
                dismiss_on_select: true,
                search_value: Some(search_val),
                ..Default::default()
            });
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select a commit to review".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search commits".to_string()),
            ..Default::default()
        });
    }

    pub fn show_review_custom_prompt(&mut self, use_reviewer: bool) {
        let tx = self.app_event_tx.clone();
        let view = CustomPromptView::new(
            "Custom review instructions".to_string(),
            "Type instructions and press Enter".to_string(),
            None,
            Box::new(move |prompt: String| {
                let trimmed = prompt.trim().to_string();
                if trimmed.is_empty() {
                    return;
                }
                let rr = ReviewRequest {
                    target: ReviewTarget::Custom {
                        instructions: trimmed,
                    },
                    user_facing_hint: None,
                    reviewer: None,
                };
                if use_reviewer {
                    tx.send(AppEvent::OpenReviewMinionPicker { review_request: rr });
                } else {
                    tx.send(AppEvent::ChaosOp(Op::Review { review_request: rr }));
                }
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
    }

    /// Show a minion persona picker. The chosen persona is set as the reviewer
    /// on `review_request` before firing the review op.
    pub fn show_review_minion_picker(&mut self, review_request: ReviewRequest) {
        let personas = chaos_kern::list_personas();
        let mut items: Vec<SelectionItem> = Vec::with_capacity(personas.len() + 1);

        {
            let rr = review_request.clone();
            items.push(SelectionItem {
                name: "Default reviewer".to_string(),
                description: Some("No persona override".into()),
                search_value: Some("default".to_string()),
                actions: vec![Box::new(move |tx: &AppEventSender| {
                    tx.send(AppEvent::ChaosOp(Op::Review {
                        review_request: rr.clone(),
                    }));
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        for (name, role) in personas {
            let mut rr = review_request.clone();
            rr.reviewer = Some(name.clone());
            let description = role.description.clone();
            let search_value = Some(format!("{name} {}", description.as_deref().unwrap_or("")));
            items.push(SelectionItem {
                name: name.clone(),
                description,
                search_value,
                actions: vec![Box::new(move |tx: &AppEventSender| {
                    tx.send(AppEvent::ChaosOp(Op::Review {
                        review_request: rr.clone(),
                    }));
                })],
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        self.bottom_pane.show_selection_view(SelectionViewParams {
            title: Some("Select a reviewer".to_string()),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            is_searchable: true,
            search_placeholder: Some("Type to search personas".to_string()),
            ..Default::default()
        });
    }
}

/// Build a `SelectionAction` that either opens the minion picker (when
/// `use_reviewer` is true) or fires the review op directly.
fn build_review_action(use_reviewer: bool, review_request: ReviewRequest) -> SelectionAction {
    if use_reviewer {
        Box::new(move |tx: &AppEventSender| {
            tx.send(AppEvent::OpenReviewMinionPicker {
                review_request: review_request.clone(),
            });
        })
    } else {
        Box::new(move |tx: &AppEventSender| {
            tx.send(AppEvent::ChaosOp(Op::Review {
                review_request: review_request.clone(),
            }));
        })
    }
}
