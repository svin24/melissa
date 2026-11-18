//! Pages and fragments, assembled only from `components`. No markup is written here.

use std::time::Duration;

use maud::{html, Markup};
use melissa_agent::{BackendInfo, Stream, TaskSnapshot, TaskStatus, MAX_PROMPT_BYTES};

use crate::components::{
    action_button, badge, card, code, link, live_region, log_view, notice, page, post_form,
    select_field, table, textarea_field, truncated, Choice, LogLine, Tone,
};

pub const TASKS_ID: &str = "tasks";
pub const TASK_ID: &str = "task";

pub fn dashboard(backends: &[BackendInfo], tasks: &[TaskSnapshot]) -> Markup {
    page(
        "Hive",
        html! {
            (card("Agents", agents_table(backends)))
            (card("New task", new_task_form(backends)))
            (card("Tasks", tasks_region(tasks, None)))
        },
    )
}

fn agents_table(backends: &[BackendInfo]) -> Markup {
    let rows = backends
        .iter()
        .map(|b| {
            vec![
                html! { (b.name) },
                html! { (b.kind) },
                if b.available {
                    badge("available", Tone::Ok)
                } else {
                    badge("not installed", Tone::Muted)
                },
                code(&b.detail),
            ]
        })
        .collect();
    table(
        &["Name", "Kind", "Status", "Command"],
        rows,
        "No agent backends configured.",
    )
}

fn new_task_form(backends: &[BackendInfo]) -> Markup {
    let choices: Vec<Choice<'_>> = backends
        .iter()
        .map(|b| Choice {
            value: &b.name,
            label: &b.name,
            enabled: b.available,
        })
        .collect();
    post_form(
        "/tasks",
        TASKS_ID,
        "Dispatch",
        html! {
            (select_field("backend", "Agent", &choices))
            (textarea_field("prompt", "Prompt", "What should the agent do?", MAX_PROMPT_BYTES))
        },
    )
}

/// The self-refreshing task list. `message` is shown above it (e.g. a dispatch error).
pub fn tasks_region(tasks: &[TaskSnapshot], message: Option<(&str, Tone)>) -> Markup {
    let rows = tasks
        .iter()
        .map(|t| {
            vec![
                link(&format!("/tasks/{}", t.id), &format!("#{}", t.id)),
                html! { (t.backend) },
                status_badge(t.status),
                truncated(&t.prompt, 60),
                html! { (format_elapsed(t.elapsed)) },
            ]
        })
        .collect();
    live_region(
        TASKS_ID,
        "/tasks",
        2,
        true,
        html! {
            @if let Some((text, tone)) = message { (notice(text, tone)) }
            (table(&["Task", "Agent", "Status", "Prompt", "Time"], rows, "No tasks yet."))
        },
    )
}

pub fn task_page(task: &TaskSnapshot) -> Markup {
    page(
        &format!("Task #{}", task.id),
        card(&format!("Task #{}", task.id), task_region(task)),
    )
}

/// The task detail region; polls itself until the task finishes.
pub fn task_region(task: &TaskSnapshot) -> Markup {
    let running = task.status == TaskStatus::Running;
    live_region(
        TASK_ID,
        &format!("/tasks/{}/fragment", task.id),
        1,
        running,
        html! {
            (table(
                &["Agent", "Status", "Time"],
                vec![vec![
                    html! { (task.backend) },
                    status_badge(task.status),
                    html! { (format_elapsed(task.elapsed)) },
                ]],
                "",
            ))
            (log_view([LogLine { text: &task.prompt, is_error: false }]))
            @if let Some(error) = &task.error { (notice(error, Tone::Error)) }
            @if task.truncated { (notice("Output was truncated.", Tone::Warn)) }
            (log_view(task.output.iter().map(|l| LogLine {
                text: &l.text,
                is_error: l.stream == Stream::Stderr,
            })))
            @if running {
                (action_button(&format!("/tasks/{}/cancel", task.id), TASK_ID, "Cancel", "Cancel this task?"))
            }
            (link("/", "Back to the hive"))
        },
    )
}

pub fn error_page(title: &str, message: &str) -> Markup {
    page(
        title,
        card(
            title,
            html! { (notice(message, Tone::Error)) (link("/", "Back to the hive")) },
        ),
    )
}

pub fn not_found() -> Markup {
    page(
        "Not found",
        card("Not found", notice("No such page or task.", Tone::Warn)),
    )
}

fn status_badge(status: TaskStatus) -> Markup {
    let tone = match status {
        TaskStatus::Running => Tone::Info,
        TaskStatus::Succeeded => Tone::Ok,
        TaskStatus::Failed => Tone::Error,
        TaskStatus::Cancelled => Tone::Muted,
    };
    badge(status.as_str(), tone)
}

fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        format!("{}m {:02}s", d.as_secs() / 60, d.as_secs() % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_formatting() {
        assert_eq!(format_elapsed(Duration::from_millis(1500)), "1.5s");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2m 05s");
    }
}
