//! The task log renders its per-row controls and can page backwards.
//!
//! Both regressed once with the unit suite green: the log rendered upside down,
//! and it could not page past its first fetch.
//! The in-place rename editor opens when its pencil is pressed.
//!
//! This shipped dead on 31 surfaces with a green unit suite either side of it,
//! and the failure is invisible to a DOM-only environment: the row around the
//! pencil is a `role="button"` that folds on `click`, the framework delegates
//! `click`, and the pencil's `stopPropagation` lost that race. jsdom has no
//! competing handler, so it never saw the conflict.

use crate::qa::{Check, Expect};

pub fn checks() -> Vec<Check> {
    vec![
        Check {
            id: "tasklog-1",
            group: "tasklog",
            what: "task log rows render their per-row copy control",
            hover: None,
            click: None,
            subject: "Copy this task-log entry",
            expect: Expect::Paints,
            press: false,
            panel_only: true,
        },
        Check {
            id: "tasklog-2",
            group: "tasklog",
            what: "revealing earlier entries adds rows",
            hover: None,
            click: Some("Show 20 earlier"),
            subject: "Copy this task-log entry",
            expect: Expect::Grows,
            press: false,
            panel_only: true,
        },
    ]
}

