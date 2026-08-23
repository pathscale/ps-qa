//! The status marker cycles a row without destroying it.
//!
//! The reported symptom was "one click appears to delete items". The cycle
//! deliberately avoids the terminal states for exactly that reason, so what
//! this pins is that a click does not remove the row.
//! Section headers are present, and collapse/expand round-trips.
//!
//! A disclosure that swaps its own label is the strongest cheap signal there
//! is: `Collapse X` must become `Expand X` and back, which is visible in the
//! tree without knowing anything about what was disclosed.
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
        /*
         * Counted over the item rows themselves, not the marker's own label.
         *
         * The marker's accessible name carries the item title and its title
         * attribute carries the status, so clicking it changes the text of the
         * node being counted and a count over "Change the status of" moves by
         * one for reasons that have nothing to do with a row disappearing.
         * `data-item-id` is the row, and it does not move when a status does.
         */
        Check {
            id: "status-1",
            group: "status",
            what: "clicking the status marker does not remove the row",
            open: Some("theta theta north indi"),
            hover: Some("Change the status of"),
            click: Some("Change the status of"),
            subject: "Edit ",
            expect: Expect::Holds,
            press: false,
            panel_only: true,
        },
        // The cycle is meant to stay inside the visible working states, so a
        // click must never park a row on a terminal one. `finished` under the
        // `delete` handling for completed items is what actually removes rows,
        // which is the shape of the "one click deletes it" report.
        Check {
            id: "status-2",
            group: "status",
            what: "the marker never cycles a row into a terminal state",
            open: Some("theta theta north indi"),
            hover: Some("Change the status of"),
            click: Some("Change the status of"),
            subject: "(Finished)",
            expect: Expect::Absent,
            press: false,
            panel_only: true,
        },
    ]
}
