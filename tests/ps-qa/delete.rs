//! A destructive control asks before it destroys.
//!
//! The reported fear was "one click appears to delete items", so what these
//! pin is the confirmation step: pressing `Delete X` must not remove the row,
//! it must grow an inline `Delete? Delete Cancel` beside it.
//!
//! Six of these were on the sweep's dead list, one per Home row. They are one
//! component, and measured on a fresh instance it works: the row's accessible
//! name becomes `…Delete?DeleteCancel` and the project stays.

use crate::qa::{Check, Expect};

pub fn checks() -> Vec<Check> {
    vec![
        Check {
            id: "delete-asks-first",
            group: "delete",
            what: "deleting a project asks before it removes anything",
            open: Some("Home"),
            hover: None,
            click: Some("Delete e756"),
            press: true,
            // The confirmation appears *in* the row, so the row's own name
            // grows the prompt. Absence of that is a delete that fired.
            subject: "Delete?",
            expect: Expect::Paints,
            panel_only: false,
        },
    ]
}
