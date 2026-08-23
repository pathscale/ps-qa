//! Section headers are present, and collapse/expand round-trips.
//!
//! A disclosure that swaps its own label is the strongest cheap signal there
//! is: `Collapse X` must become `Expand X` and back, which is visible in the
//! tree without knowing anything about what was disclosed.
//! The task log renders its per-row controls and can page backwards.
//!
//! Both regressed once with the unit suite green: the log rendered upside down,
//! and it could not page past its first fetch.

use crate::qa::{Check, Expect};

pub fn checks() -> Vec<Check> {
    vec![
        Check {
            id: "sections-1",
            group: "sections",
            what: "the Items section header is on screen",
            open: Some("alpha sigma omega west"),
            hover: None,
            click: None,
            subject: "Items",
            expect: Expect::Paints,
            press: false,
            panel_only: true,
        },
        Check {
            id: "sections-2",
            group: "sections",
            what: "the Task log section header is on screen",
            open: Some("alpha sigma omega west"),
            hover: None,
            click: None,
            subject: "Task log",
            expect: Expect::Paints,
            press: false,
            panel_only: true,
        },
        Check {
            id: "sections-3",
            group: "sections",
            what: "the Agent I/O section header is on screen",
            open: Some("alpha sigma omega west"),
            hover: None,
            click: None,
            subject: "Agent I/O",
            expect: Expect::Paints,
            press: false,
            panel_only: true,
        },
        Check {
            id: "sections-4",
            group: "sections",
            what: "collapsing a section is acknowledged",
            open: Some("alpha sigma omega west"),
            hover: None,
            click: Some("Collapse Task log"),
            subject: "Expand Task log",
            expect: Expect::Paints,
            press: false,
            panel_only: true,
        },
        Check {
            id: "sections-5",
            group: "sections",
            what: "expanding it again restores the control",
            open: Some("alpha sigma omega west"),
            hover: None,
            click: Some("Expand Task log"),
            subject: "Collapse Task log",
            expect: Expect::Paints,
            press: false,
            panel_only: true,
        },
    ]
}

