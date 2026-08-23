//! Controls that do not exist until the pointer is on their row.
//!
//! A check that forgets the hover reports "no such node" and reads as a missing
//! feature rather than a test driving the app wrongly.
//!
//! The row to hover has to be a *panel* row. Home renders the same control
//! names and its rows carry no reorder arrows, so hovering whichever row
//! matched first put the pointer on Home and reported the panel's arrows as
//! broken when they were never asked to appear. That mistake cost a round of
//! chasing a bug that did not exist, which is why the runner targets a point
//! inside the column instead of a name that both lists answer to.
//! The status marker cycles a row without destroying it.
//!
//! The reported symptom was "one click appears to delete items". The cycle
//! deliberately avoids the terminal states for exactly that reason, so what
//! this pins is that a click does not remove the row.
//!
//! # These need a project with items, and the pristine profile has none
//!
//! Every project in the QA fixture reports `Items0`, so the row controls these
//! assert on are never built and the checks fail with "no node matching". That
//! is a fixture gap, not an application fault: the controls cannot be judged
//! either way until the profile has a project with at least one item.
//!
//! Fix the fixture rather than the checks - deleting them would lose the only
//! coverage of the panel's row controls, which is where the owner reports most
//! problems.

use crate::qa::{Check, Expect};

pub fn checks() -> Vec<Check> {
    vec![
        Check {
            id: "hover-1",
            group: "hover",
            what: "hovering an item row reveals its move-up arrow",
            open: Some("eno working directory"),
            hover: Some("Change the status of"),
            click: None,
            subject: "Move ",
            expect: Expect::Paints,
            press: false,
            panel_only: true,
        },
        Check {
            id: "hover-2",
            group: "hover",
            what: "hovering an item row reveals its edit control",
            open: Some("eno working directory"),
            hover: Some("Change the status of"),
            click: None,
            subject: "Edit ",
            expect: Expect::Paints,
            press: false,
            panel_only: true,
        },
        Check {
            id: "hover-3",
            group: "hover",
            what: "hovering an item row reveals its delete control",
            open: Some("eno working directory"),
            hover: Some("Change the status of"),
            click: None,
            subject: "Delete ",
            expect: Expect::Paints,
            press: false,
            panel_only: true,
        },
    ]
}

