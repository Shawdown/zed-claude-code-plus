use acp_thread::{
    AgentSessionBackgroundTasks, BackgroundTaskOutput, BackgroundTaskState, BackgroundTaskStatus,
};
use gpui::DismissEvent;

use super::*;

const TICK: Duration = Duration::from_secs(1);

pub(crate) fn format_task_duration(milliseconds: i64) -> String {
    let seconds = milliseconds.max(0) / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

pub(crate) fn task_elapsed(task: &BackgroundTaskState, now: i64) -> i64 {
    task.ended_at.unwrap_or(now) - task.started_at
}

pub(crate) fn task_name(task: &BackgroundTaskState) -> SharedString {
    task.description
        .as_deref()
        .or(task.command.as_deref())
        .or(task.task_type.as_deref())
        .unwrap_or(task.task_id.as_str())
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
        .into()
}

fn status_label(task: &BackgroundTaskState) -> (&'static str, Color) {
    match task.status {
        BackgroundTaskStatus::Running => ("running", Color::Accent),
        BackgroundTaskStatus::Completed => ("done", Color::Success),
        BackgroundTaskStatus::Failed => ("failed", Color::Error),
        BackgroundTaskStatus::Stopped => ("stopped", Color::Warning),
        BackgroundTaskStatus::Ended => ("ended", Color::Muted),
    }
}

fn status_description(task: &BackgroundTaskState) -> &'static str {
    match task.status {
        BackgroundTaskStatus::Running => "still running",
        BackgroundTaskStatus::Completed => "completed",
        BackgroundTaskStatus::Failed => "failed",
        BackgroundTaskStatus::Stopped if task.stopped_by_client => "stopped by you",
        BackgroundTaskStatus::Stopped => "stopped",
        BackgroundTaskStatus::Ended => "ended",
    }
}

pub(crate) fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

enum OutputState {
    Loading,
    Loaded(BackgroundTaskOutput),
    Failed(SharedString),
}

pub struct BackgroundTasksPopover {
    thread: Entity<AcpThread>,
    thread_view: WeakEntity<ThreadView>,
    controls: Option<Rc<dyn AgentSessionBackgroundTasks>>,
    focus_handle: FocusHandle,
    expanded: HashSet<String>,
    focused_task: Option<String>,
    outputs: HashMap<String, OutputState>,
    stopping: HashSet<String>,
    errors: HashMap<String, SharedString>,
    _tick: Option<Task<()>>,
    _output_tasks: HashMap<String, Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl BackgroundTasksPopover {
    pub(crate) fn new(
        thread: Entity<AcpThread>,
        thread_view: WeakEntity<ThreadView>,
        expand_task: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let controls = {
            let thread = thread.read(cx);
            thread
                .connection()
                .background_tasks(thread.session_id(), cx)
        };
        let subscriptions = vec![cx.observe(&thread, |_, _, cx| cx.notify())];
        let mut this = Self {
            thread,
            thread_view,
            controls,
            focus_handle: cx.focus_handle(),
            expanded: HashSet::default(),
            focused_task: expand_task.clone(),
            outputs: HashMap::default(),
            stopping: HashSet::default(),
            errors: HashMap::default(),
            _tick: None,
            _output_tasks: HashMap::default(),
            _subscriptions: subscriptions,
        };
        if let Some(task_id) = expand_task {
            this.toggle_expanded(task_id, cx);
        }
        this
    }

    fn toggle_expanded(&mut self, task_id: String, cx: &mut Context<Self>) {
        if self.expanded.remove(&task_id) {
            self._output_tasks.remove(&task_id);
        } else {
            self.expanded.insert(task_id.clone());
            self.fetch_output(task_id, cx);
        }
        cx.notify();
    }

    fn fetch_output(&mut self, task_id: String, cx: &mut Context<Self>) {
        let Some(controls) = self.controls.clone() else {
            return;
        };
        self.outputs.insert(task_id.clone(), OutputState::Loading);
        let request = controls.output(task_id.clone(), cx);
        let task = cx.spawn({
            let task_id = task_id.clone();
            async move |this, cx| {
                let state = match request.await {
                    Ok(output) => OutputState::Loaded(output),
                    Err(error) => {
                        log::error!("Failed to read background task output: {error:#}");
                        OutputState::Failed(format!("Could not read the output: {error}").into())
                    }
                };
                this.update(cx, |this, cx| {
                    this.outputs.insert(task_id.clone(), state);
                    this._output_tasks.remove(&task_id);
                    cx.notify();
                })
                .log_err();
            }
        });
        self._output_tasks.insert(task_id, task);
    }

    fn stop(&mut self, task_id: String, cx: &mut Context<Self>) {
        let Some(controls) = self.controls.clone() else {
            return;
        };
        if !self.stopping.insert(task_id.clone()) {
            return;
        }
        self.errors.remove(&task_id);
        cx.notify();
        let request = controls.stop(task_id.clone(), cx);
        cx.spawn(async move |this, cx| {
            let error = match request.await {
                Ok(true) => None,
                Ok(false) => Some(SharedString::from("The task was no longer running.")),
                Err(error) => {
                    log::error!("Failed to stop background task: {error:#}");
                    Some(SharedString::from(format!(
                        "Could not stop the task: {error}"
                    )))
                }
            };
            this.update(cx, |this, cx| {
                this.stopping.remove(&task_id);
                if let Some(error) = error {
                    this.errors.insert(task_id.clone(), error);
                    if !this.expanded.contains(&task_id) {
                        this.toggle_expanded(task_id, cx);
                    }
                }
                cx.notify();
            })
        })
        .detach_and_log_err(cx);
    }

    fn jump_to_entry(&mut self, entry_ix: usize, cx: &mut Context<Self>) {
        self.thread_view
            .update(cx, |thread_view, cx| {
                thread_view.list_state.scroll_to(ListOffset {
                    item_ix: entry_ix,
                    offset_in_item: px(0.),
                });
                cx.notify();
            })
            .log_err();
        cx.emit(DismissEvent);
    }

    fn schedule_tick(&mut self, any_running: bool, cx: &mut Context<Self>) {
        if !any_running {
            self._tick = None;
            return;
        }
        if self._tick.is_some() {
            return;
        }
        self._tick = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TICK).await;
            this.update(cx, |this, cx| {
                this._tick = None;
                cx.notify();
            })
            .ok();
        }));
    }

    fn render_task(
        &self,
        task: &BackgroundTaskState,
        now: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let thread = self.thread.read(cx);
        let origin_ix = thread.background_task_entry_index(&task.tool_call_id);
        let reply_ix = (!task.is_running())
            .then(|| thread.background_task_reply_entry_index(&task.task_id))
            .flatten();
        let is_expanded = self.expanded.contains(&task.task_id);
        let is_stopping = self.stopping.contains(&task.task_id);
        let (status, status_color) = status_label(task);
        let elapsed = format_task_duration(task_elapsed(task, now));
        let task_id = task.task_id.clone();
        let element_id =
            |suffix: &str| SharedString::from(format!("background-task-{suffix}-{}", task.task_id));

        let row = h_flex()
            .id(element_id("row"))
            .w_full()
            .min_w_0()
            .h_7()
            .px_1()
            .gap_1p5()
            .rounded_sm()
            .cursor_pointer()
            .hover(|style| style.bg(cx.theme().colors().element_hover))
            .when_some(task.command.clone(), |this, command| {
                this.tooltip(Tooltip::text(command))
            })
            .on_click(cx.listener({
                let task_id = task_id.clone();
                move |this, _, _, cx| this.toggle_expanded(task_id.clone(), cx)
            }))
            .child(
                Icon::new(if is_expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(IconSize::Small)
                .color(Color::Muted),
            )
            .child(
                Label::new(status)
                    .size(LabelSize::XSmall)
                    .color(status_color),
            )
            .child(
                div().flex_1().min_w_0().child(
                    Label::new(task_name(task))
                        .size(LabelSize::Small)
                        .buffer_font(cx)
                        .single_line()
                        .truncate(),
                ),
            )
            .child(
                Label::new(elapsed.clone())
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_0p5()
                    .when_some(origin_ix, |this, entry_ix| {
                        this.child(
                            IconButton::new(element_id("origin"), IconName::ArrowUp)
                                .icon_size(IconSize::Small)
                                .icon_color(Color::Muted)
                                .tooltip(Tooltip::text("Jump to where it started"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.jump_to_entry(entry_ix, cx);
                                })),
                        )
                    })
                    .when(task.is_running() && self.controls.is_some(), |this| {
                        let task_id = task_id.clone();
                        this.child(
                            IconButton::new(element_id("stop"), IconName::Stop)
                                .icon_size(IconSize::Small)
                                .icon_color(Color::Error)
                                .disabled(is_stopping)
                                .tooltip(Tooltip::text(if is_stopping {
                                    "Stopping…"
                                } else {
                                    "Stop task"
                                }))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.stop(task_id.clone(), cx);
                                })),
                        )
                    })
                    .when_some(reply_ix, |this, entry_ix| {
                        this.child(
                            IconButton::new(element_id("reply"), IconName::ArrowDown)
                                .icon_size(IconSize::Small)
                                .icon_color(Color::Muted)
                                .tooltip(Tooltip::text("Jump to the reply about it"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.jump_to_entry(entry_ix, cx);
                                })),
                        )
                    }),
            );

        v_flex()
            .w_full()
            .child(row)
            .when(is_expanded, |this| {
                this.child(self.render_details(task, &elapsed, cx))
            })
            .into_any_element()
    }

    fn render_details(
        &self,
        task: &BackgroundTaskState,
        elapsed: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let section_label =
            |text: &'static str| Label::new(text).size(LabelSize::XSmall).color(Color::Muted);
        let code_block = |cx: &App| {
            div()
                .w_full()
                .min_w_0()
                .p_1p5()
                .rounded_sm()
                .bg(cx.theme().colors().editor_background)
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .font_buffer(cx)
                .text_xs()
        };
        let meta = if task.is_running() {
            format!("started {elapsed} ago · {}", status_description(task))
        } else {
            format!("ran {elapsed} · {}", status_description(task))
        };

        let output = match self.outputs.get(&task.task_id) {
            None => None,
            Some(OutputState::Loading) => Some(
                Label::new("Loading…")
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .into_any_element(),
            ),
            Some(OutputState::Failed(message)) => Some(
                Label::new(message.clone())
                    .size(LabelSize::Small)
                    .color(Color::Error)
                    .into_any_element(),
            ),
            Some(OutputState::Loaded(output)) if output.output.trim().is_empty() => Some(
                Label::new("No output")
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .into_any_element(),
            ),
            Some(OutputState::Loaded(output)) => Some(
                code_block(cx)
                    .id(SharedString::from(format!(
                        "background-task-output-{}",
                        task.task_id
                    )))
                    .max_h_48()
                    .overflow_y_scroll()
                    .whitespace_normal()
                    .child(SharedString::from(output.output.clone()))
                    .into_any_element(),
            ),
        };
        let truncated = matches!(
            self.outputs.get(&task.task_id),
            Some(OutputState::Loaded(output)) if output.truncated
        );

        v_flex()
            .w_full()
            .min_w_0()
            .pl_6()
            .pr_1()
            .pt_0p5()
            .pb_2()
            .gap_1()
            .when_some(task.command.clone(), |this, command| {
                this.child(section_label("Command")).child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .items_start()
                        .gap_1()
                        .child(
                            code_block(cx)
                                .flex_1()
                                .whitespace_normal()
                                .child(SharedString::from(command.clone())),
                        )
                        .child(
                            IconButton::new(
                                SharedString::from(format!(
                                    "background-task-copy-{}",
                                    task.task_id
                                )),
                                IconName::Copy,
                            )
                            .icon_size(IconSize::Small)
                            .icon_color(Color::Muted)
                            .tooltip(Tooltip::text("Copy command"))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(command.clone()));
                            }),
                        ),
                )
            })
            .child(Label::new(meta).size(LabelSize::XSmall).color(Color::Muted))
            .when_some(task.summary.clone(), |this, summary| {
                this.child(Label::new(summary).size(LabelSize::Small))
            })
            .when_some(self.errors.get(&task.task_id).cloned(), |this, error| {
                this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
            })
            .when_some(output, |this, output| {
                this.child(section_label(if truncated {
                    "Output (last 8 KiB)"
                } else {
                    "Output"
                }))
                .child(output)
            })
            .into_any_element()
    }
}

impl Render for BackgroundTasksPopover {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = now_millis();
        let (running, focused_finished) = {
            let thread = self.thread.read(cx);
            let running: Vec<BackgroundTaskState> = thread
                .background_tasks()
                .filter(|task| task.is_running())
                .cloned()
                .collect();
            let focused_finished = self.focused_task.as_ref().and_then(|task_id| {
                thread
                    .background_tasks()
                    .find(|task| &task.task_id == task_id && !task.is_running())
                    .cloned()
            });
            (running, focused_finished)
        };
        self.schedule_tick(!running.is_empty(), cx);

        let section = |title: &'static str| {
            div().pt_1p5().pb_0p5().px_1().child(
                Label::new(title)
                    .size(LabelSize::XSmall)
                    .color(Color::Muted),
            )
        };

        v_flex()
            .id("background-tasks-popover")
            .key_context("BackgroundTasksPopover")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .w_96()
            .max_h_96()
            .overflow_y_scroll()
            .p_1()
            .elevation_2(cx)
            .child(
                h_flex()
                    .px_1()
                    .pb_1()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(Label::new("Background tasks").size(LabelSize::Small))
                    .child(
                        Label::new(format!("{} running", running.len()))
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
            )
            .when(running.is_empty() && focused_finished.is_none(), |this| {
                this.child(
                    div().px_1().py_2().child(
                        Label::new("No background tasks running")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    ),
                )
            })
            .when(!running.is_empty(), |this| {
                this.child(section("Running")).children(
                    running
                        .iter()
                        .map(|task| self.render_task(task, now, cx))
                        .collect::<Vec<_>>(),
                )
            })
            .when_some(focused_finished, |this, task| {
                this.child(self.render_task(&task, now, cx))
            })
    }
}

impl Focusable for BackgroundTasksPopover {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for BackgroundTasksPopover {}
