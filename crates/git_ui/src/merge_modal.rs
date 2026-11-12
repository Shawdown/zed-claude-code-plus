use std::time::Duration;

use editor::{Editor, EditorEvent};
use git::operation::{
    ApplyCommitsOptions, ConflictPreference, FastForward, GitOperation, GitOperationPreview,
    GitOperationPreviewRequest, MergeOptions,
};
use gpui::{
    AnyElement, App, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, SharedString,
    Subscription, Task, WeakEntity, Window,
};
use menu::{Cancel, Confirm};
use project::git_store::Repository;
use ui::{
    Checkbox, KeyBinding, ToggleButtonGroup, ToggleButtonGroupStyle, ToggleButtonSimple,
    ToggleState, prelude::*,
};
use workspace::{ModalView, Workspace};

use crate::git_panel::GitPanel;

const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(250);
const MAX_LISTED_COMMITS: usize = 8;
const MAX_LISTED_CONFLICTS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MergeModalMode {
    Merge,
    Squash,
    CherryPick,
}

impl MergeModalMode {
    fn action_label(self) -> &'static str {
        match self {
            Self::Merge => "Merge",
            Self::Squash => "Squash Merge",
            Self::CherryPick => "Cherry-pick",
        }
    }
}

struct Preview {
    request: GitOperationPreviewRequest,
    result: Result<GitOperationPreview, SharedString>,
}

pub(crate) struct MergeModal {
    mode: MergeModalMode,
    source_editor: Entity<Editor>,
    repository: Entity<Repository>,
    git_panel: WeakEntity<GitPanel>,
    head_name: SharedString,
    fast_forward: FastForward,
    commit_merge: bool,
    prefer: Option<ConflictPreference>,
    allow_unrelated_histories: bool,
    apply_options: ApplyCommitsOptions,
    signoff: bool,
    preview: Option<Preview>,
    preview_task: Option<Task<()>>,
    _source_subscription: Subscription,
}

/// Opens the modal for `repository`, or the Git Panel's active repository.
pub(crate) fn open_merge_modal(
    workspace: &mut Workspace,
    repository: Option<Entity<Repository>>,
    mode: MergeModalMode,
    source: Option<String>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(git_panel) = workspace.panel::<GitPanel>(cx) else {
        return;
    };
    let Some(repository) = repository.or_else(|| git_panel.read(cx).active_repository.clone())
    else {
        return;
    };
    let git_panel = git_panel.downgrade();
    workspace.toggle_modal(window, cx, |window, cx| {
        MergeModal::new(mode, source, repository, git_panel, window, cx)
    });
}

/// Runs `operation` through the Git Panel, which reports the outcome.
pub(crate) fn run_git_operation(
    workspace: &WeakEntity<Workspace>,
    repository: Entity<Repository>,
    operation: GitOperation,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(git_panel) = workspace
        .upgrade()
        .and_then(|workspace| workspace.read(cx).panel::<GitPanel>(cx))
    else {
        return;
    };
    git_panel.update(cx, |git_panel, cx| {
        git_panel.run_git_operation(repository, operation, window, cx)
    });
}

/// The name of the branch operations apply to: the current branch, or the short HEAD sha when detached.
pub(crate) fn head_name(repository: &Repository) -> SharedString {
    repository
        .branch
        .as_ref()
        .map(|branch| SharedString::from(branch.name().to_string()))
        .or_else(|| {
            repository
                .head_commit
                .as_ref()
                .map(|commit| commit.sha.chars().take(7).collect::<String>().into())
        })
        .unwrap_or_else(|| "HEAD".into())
}

impl MergeModal {
    fn new(
        mode: MergeModalMode,
        source: Option<String>,
        repository: Entity<Repository>,
        git_panel: WeakEntity<GitPanel>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let source_editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            if let Some(source) = source {
                editor.set_text(source, window, cx);
            }
            editor
        });
        let source_subscription = cx.subscribe_in(
            &source_editor,
            window,
            |this, _, event: &EditorEvent, _, cx| {
                if let EditorEvent::BufferEdited = event {
                    this.request_preview(cx);
                }
            },
        );

        let head_name = head_name(repository.read(cx));

        let mut this = Self {
            mode,
            source_editor,
            repository,
            git_panel,
            head_name,
            fast_forward: FastForward::Auto,
            commit_merge: true,
            prefer: None,
            allow_unrelated_histories: false,
            apply_options: ApplyCommitsOptions::default(),
            signoff: false,
            preview: None,
            preview_task: None,
            _source_subscription: source_subscription,
        };
        this.update_placeholder(window, cx);
        this.request_preview(cx);
        this
    }

    fn update_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = match self.mode {
            MergeModalMode::Merge | MergeModalMode::Squash => "Branch, tag or commit",
            MergeModalMode::CherryPick => "Commits or a range, e.g. abc123 def456 or main..feature",
        };
        self.source_editor.update(cx, |editor, cx| {
            editor.set_placeholder_text(placeholder, window, cx)
        });
    }

    fn set_mode(&mut self, mode: MergeModalMode, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.update_placeholder(window, cx);
        self.request_preview(cx);
        cx.notify();
    }

    fn revisions(&self, cx: &App) -> Vec<String> {
        self.source_editor
            .read(cx)
            .text(cx)
            .split_whitespace()
            .map(str::to_string)
            .collect()
    }

    fn preview_request(&self, cx: &App) -> Option<GitOperationPreviewRequest> {
        let revisions = self.revisions(cx);
        if revisions.is_empty() {
            return None;
        }
        Some(match self.mode {
            MergeModalMode::Merge | MergeModalMode::Squash => GitOperationPreviewRequest::Merge {
                source: revisions.join(" "),
            },
            MergeModalMode::CherryPick => GitOperationPreviewRequest::CherryPick {
                revisions,
                mainline: self.apply_options.mainline,
            },
        })
    }

    fn request_preview(&mut self, cx: &mut Context<Self>) {
        let Some(request) = self.preview_request(cx) else {
            self.preview = None;
            self.preview_task = None;
            cx.notify();
            return;
        };
        if self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.request == request)
        {
            return;
        }
        let repository = self.repository.clone();
        self.preview_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PREVIEW_DEBOUNCE).await;
            let receiver = repository.update(cx, |repository, _| {
                repository.preview_git_operation(request.clone())
            });
            let result = receiver
                .await
                .unwrap_or_else(|canceled| Err(canceled.into()))
                .map_err(|error| SharedString::from(error.to_string()));
            this.update(cx, |this, cx| {
                this.preview = Some(Preview { request, result });
                this.preview_task = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The preview, when it matches the current input.
    fn current_preview(&self, cx: &App) -> Option<&GitOperationPreview> {
        let preview = self.preview.as_ref()?;
        if Some(&preview.request) != self.preview_request(cx).as_ref() {
            return None;
        }
        preview.result.as_ref().ok()
    }

    fn operation(&self, cx: &App) -> Option<GitOperation> {
        let revisions = self.revisions(cx);
        if revisions.is_empty() {
            return None;
        }
        Some(match self.mode {
            MergeModalMode::Merge | MergeModalMode::Squash => {
                let squash = self.mode == MergeModalMode::Squash;
                GitOperation::Merge(MergeOptions {
                    fast_forward: self.fast_forward,
                    squash,
                    commit: squash || self.commit_merge,
                    signoff: self.signoff,
                    prefer: self.prefer,
                    allow_unrelated_histories: self.allow_unrelated_histories,
                    ..MergeOptions::new(revisions.join(" "))
                })
            }
            MergeModalMode::CherryPick => {
                // Git walks ancestors when ranges and single commits are mixed, so those run as the
                // commit list the preview resolved.
                let has_range = revisions.iter().any(|revision| revision.contains(".."));
                let commits = if has_range && revisions.len() > 1 {
                    self.current_preview(cx)?
                        .commits
                        .iter()
                        .map(|commit| commit.sha.to_string())
                        .collect()
                } else {
                    revisions
                };
                GitOperation::CherryPick {
                    commits,
                    options: ApplyCommitsOptions {
                        signoff: self.signoff,
                        ..self.apply_options.clone()
                    },
                }
            }
        })
    }

    /// Why the operation can't run yet, if it can't.
    fn blocker(&self, cx: &App) -> Option<SharedString> {
        self.preview_request(cx)?;
        if let Some(preview) = &self.preview
            && Some(&preview.request) == self.preview_request(cx).as_ref()
            && let Err(error) = &preview.result
        {
            return Some(error.clone());
        }
        let preview = self.current_preview(cx)?;
        match self.mode {
            MergeModalMode::Merge | MergeModalMode::Squash if preview.up_to_date => {
                Some("Already up to date".into())
            }
            MergeModalMode::Merge
                if self.fast_forward == FastForward::Only && !preview.can_fast_forward =>
            {
                Some(format!("{} can't be fast-forwarded", self.head_name).into())
            }
            MergeModalMode::CherryPick if preview.commits.is_empty() => {
                Some("No commits to apply".into())
            }
            _ => None,
        }
    }

    fn can_run(&self, cx: &App) -> bool {
        self.current_preview(cx).is_some() && self.blocker(cx).is_none()
    }

    fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn confirm(&mut self, _: &Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_run(cx) {
            return;
        }
        let Some(operation) = self.operation(cx) else {
            return;
        };
        let repository = self.repository.clone();
        self.git_panel
            .update(cx, |git_panel, cx| {
                git_panel.run_git_operation(repository, operation, window, cx)
            })
            .ok();
        cx.emit(DismissEvent);
    }

    fn render_mode_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let modes = [
            MergeModalMode::Merge,
            MergeModalMode::Squash,
            MergeModalMode::CherryPick,
        ];
        ToggleButtonGroup::single_row(
            "merge-modal-mode",
            modes.map(|mode| {
                ToggleButtonSimple::new(
                    match mode {
                        MergeModalMode::Merge => "Merge",
                        MergeModalMode::Squash => "Squash",
                        MergeModalMode::CherryPick => "Cherry-pick",
                    },
                    cx.listener(move |this, _, window, cx| this.set_mode(mode, window, cx)),
                )
            }),
        )
        .style(ToggleButtonGroupStyle::Outlined)
        .auto_width()
        .selected_index(
            modes
                .iter()
                .position(|mode| *mode == self.mode)
                .unwrap_or(0),
        )
    }

    fn checkbox(
        id: &'static str,
        label: &'static str,
        checked: bool,
        cx: &mut Context<Self>,
        update: impl Fn(&mut Self, bool) + 'static,
    ) -> AnyElement {
        let this = cx.weak_entity();
        Checkbox::new(id, checked.into())
            .label(label)
            .label_size(LabelSize::Small)
            .on_click(move |state, _, cx| {
                let checked = *state == ToggleState::Selected;
                this.update(cx, |this, cx| {
                    update(this, checked);
                    this.request_preview(cx);
                    cx.notify();
                })
                .ok();
            })
            .into_any_element()
    }

    fn option_row<const COUNT: usize, T: Copy + PartialEq + 'static>(
        id: &'static str,
        label: &'static str,
        choices: [(&'static str, T); COUNT],
        selected: T,
        cx: &mut Context<Self>,
        update: impl Fn(&mut Self, T) + Clone + 'static,
    ) -> AnyElement {
        h_flex()
            .gap_2()
            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            .child(
                ToggleButtonGroup::single_row(
                    id,
                    choices.map(|(choice_label, value)| {
                        let update = update.clone();
                        ToggleButtonSimple::new(
                            choice_label,
                            cx.listener(move |this, _, _, cx| {
                                update(this, value);
                                this.request_preview(cx);
                                cx.notify();
                            }),
                        )
                    }),
                )
                .style(ToggleButtonGroupStyle::Outlined)
                .label_size(LabelSize::Small)
                .auto_width()
                .selected_index(
                    choices
                        .iter()
                        .position(|(_, value)| *value == selected)
                        .unwrap_or(0),
                ),
            )
            .into_any_element()
    }

    fn render_options(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let conflicts_row = Self::option_row(
            "merge-modal-prefer",
            "On conflicts",
            [
                ("Stop", None),
                ("Prefer ours", Some(ConflictPreference::Ours)),
                ("Prefer theirs", Some(ConflictPreference::Theirs)),
            ],
            self.prefer,
            cx,
            |this, prefer| this.prefer = prefer,
        );
        let signoff = Self::checkbox(
            "merge-modal-signoff",
            "Sign off",
            self.signoff,
            cx,
            |this, checked| this.signoff = checked,
        );

        match self.mode {
            MergeModalMode::Merge | MergeModalMode::Squash => v_flex()
                .gap_1p5()
                .when(self.mode == MergeModalMode::Merge, |options| {
                    options.child(Self::option_row(
                        "merge-modal-fast-forward",
                        "Fast-forward",
                        [
                            ("When possible", FastForward::Auto),
                            ("Only", FastForward::Only),
                            ("Never", FastForward::Never),
                        ],
                        self.fast_forward,
                        cx,
                        |this, fast_forward| this.fast_forward = fast_forward,
                    ))
                })
                .child(conflicts_row)
                .child(
                    h_flex()
                        .gap_3()
                        .when(self.mode == MergeModalMode::Merge, |row| {
                            row.child(Self::checkbox(
                                "merge-modal-commit",
                                "Commit result",
                                self.commit_merge,
                                cx,
                                |this, checked| this.commit_merge = checked,
                            ))
                        })
                        .child(signoff)
                        .child(Self::checkbox(
                            "merge-modal-unrelated",
                            "Allow unrelated histories",
                            self.allow_unrelated_histories,
                            cx,
                            |this, checked| this.allow_unrelated_histories = checked,
                        )),
                ),
            MergeModalMode::CherryPick => v_flex()
                .gap_1p5()
                .child(Self::option_row(
                    "merge-modal-mainline",
                    "Merge commits against parent",
                    [("None", None), ("1", Some(1)), ("2", Some(2))],
                    self.apply_options.mainline,
                    cx,
                    |this, mainline| this.apply_options.mainline = mainline,
                ))
                .child(
                    h_flex()
                        .gap_3()
                        .child(Self::checkbox(
                            "merge-modal-commit-each",
                            "Commit each",
                            self.apply_options.commit,
                            cx,
                            |this, checked| this.apply_options.commit = checked,
                        ))
                        .child(Self::checkbox(
                            "merge-modal-record-origin",
                            "Record origin (-x)",
                            self.apply_options.record_origin,
                            cx,
                            |this, checked| this.apply_options.record_origin = checked,
                        ))
                        .child(signoff),
                ),
        }
    }

    fn render_forecast(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if let Some(blocker) = self.blocker(cx) {
            return Some(
                v_flex().child(
                    Label::new(blocker)
                        .size(LabelSize::Small)
                        .color(Color::Warning),
                ),
            );
        }
        let Some(preview) = self.current_preview(cx) else {
            if self.preview_request(cx).is_some() {
                return Some(
                    v_flex().child(
                        Label::new("Checking…")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                );
            }
            return None;
        };

        let plural = |count: usize, noun: &str| {
            format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
        };
        let mut summary = vec![plural(preview.commit_count, "commit")];
        if let Some(changed_files) = preview.changed_files {
            summary.push(format!("{} changed", plural(changed_files, "file")));
        }
        let fast_forwards = self.mode == MergeModalMode::Merge
            && preview.can_fast_forward
            && self.fast_forward != FastForward::Never;
        if fast_forwards {
            summary.push("fast-forward".into());
        }

        let (conflict_text, conflict_color) = match &preview.conflicted_paths {
            None => (
                "Conflict forecast needs git 2.40 or newer".to_string(),
                Color::Muted,
            ),
            Some(paths) if paths.is_empty() => ("No conflicts".to_string(), Color::Success),
            Some(paths) => {
                let mut text = format!(
                    "{}: {}",
                    plural(paths.len(), "conflict"),
                    paths
                        .iter()
                        .take(MAX_LISTED_CONFLICTS)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                if paths.len() > MAX_LISTED_CONFLICTS {
                    text.push_str(&format!(" +{} more", paths.len() - MAX_LISTED_CONFLICTS));
                }
                if self.prefer.is_some() && self.mode != MergeModalMode::CherryPick {
                    text.push_str(" (before preferring a side)");
                }
                (text, Color::Warning)
            }
        };

        let commit_list = (self.mode == MergeModalMode::CherryPick).then(|| {
            v_flex()
                .gap_0p5()
                .children(
                    preview
                        .commits
                        .iter()
                        .take(MAX_LISTED_COMMITS)
                        .map(|commit| {
                            h_flex()
                                .gap_2()
                                .min_w_0()
                                .child(
                                    Label::new(commit.sha.chars().take(7).collect::<String>())
                                        .size(LabelSize::Small)
                                        .color(Color::Muted)
                                        .buffer_font(cx),
                                )
                                .child(
                                    Label::new(commit.subject.clone())
                                        .size(LabelSize::Small)
                                        .truncate(),
                                )
                        }),
                )
                .when(preview.commits.len() > MAX_LISTED_COMMITS, |list| {
                    list.child(
                        Label::new(format!(
                            "+{} more",
                            preview.commits.len() - MAX_LISTED_COMMITS
                        ))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    )
                })
        });

        Some(
            v_flex()
                .gap_1()
                .children(commit_list)
                .child(
                    Label::new(summary.join(" · "))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .when(!fast_forwards, |forecast| {
                    forecast.child(
                        Label::new(conflict_text)
                            .size(LabelSize::Small)
                            .color(conflict_color),
                    )
                }),
        )
    }
}

impl EventEmitter<DismissEvent> for MergeModal {}
impl ModalView for MergeModal {}
impl Focusable for MergeModal {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.source_editor.focus_handle(cx)
    }
}

impl Render for MergeModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match self.mode {
            MergeModalMode::Merge | MergeModalMode::Squash => {
                format!("Merge into {}", self.head_name)
            }
            MergeModalMode::CherryPick => format!("Cherry-pick onto {}", self.head_name),
        };
        let command_line = self
            .operation(cx)
            .and_then(|operation| operation.command_line());
        let can_run = self.can_run(cx);
        let focus_handle = self.source_editor.focus_handle(cx);

        v_flex()
            .key_context("MergeModal")
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::confirm))
            .elevation_2(cx)
            .w(rems(38.))
            .child(
                h_flex()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .w_full()
                    .gap_1p5()
                    .justify_between()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(Icon::new(IconName::GitBranch).size(IconSize::XSmall))
                            .child(Headline::new(title).size(HeadlineSize::XSmall)),
                    )
                    .child(self.render_mode_toggle(cx)),
            )
            .child(
                v_flex()
                    .px_3()
                    .pb_3()
                    .gap_2()
                    .child(
                        div()
                            .px_2()
                            .py_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(cx.theme().colors().border)
                            .child(self.source_editor.clone()),
                    )
                    .child(self.render_options(cx))
                    .children(self.render_forecast(cx))
                    .children(command_line.map(|command_line| {
                        Label::new(command_line)
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .buffer_font(cx)
                    })),
            )
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .justify_end()
                    .border_t_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        Button::new("merge-modal-cancel", "Cancel")
                            .key_binding(KeyBinding::for_action_in(&Cancel, &focus_handle, cx))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.cancel(&Cancel, window, cx)),
                            ),
                    )
                    .child(
                        Button::new("merge-modal-run", self.mode.action_label())
                            .style(ButtonStyle::Filled)
                            .disabled(!can_run)
                            .key_binding(KeyBinding::for_action_in(&Confirm, &focus_handle, cx))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm(&Confirm, window, cx)
                            })),
                    ),
            )
    }
}
