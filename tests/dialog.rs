//! A dialog opens *and* can be dismissed.
//!
//! Two halves, in order, and the order matters: the checks run in sequence
//! against one instance, so the second inherits the dialog the first opened.
//! Splitting them is what makes a failure legible - "it never opened" and "it
//! opened and would not close" are different bugs, and the one that shipped
//! was the second. It trapped the window and put 68 of that surface's 84
//! controls out of reach behind one dialog.
//! The side panel's left edge, in window coordinates.
//!
//! The panel is a fixed 332px column on the right, and Home renders its own
//! item list with the same control names. Counting across the whole window
//! therefore mixes two lists: "Edit " matched 107 nodes and "Copy this
//! task-log entry" matched 880, most of them Home's, so a panel row appearing
//! or leaving was lost in the noise. Anything left of this is not the panel.
//! Nodes matching `want`, by accessible name or role, inside the side panel.
//!
//! Nodes with no box are kept: a control that is in the tree with no geometry
//! is exactly the failure [`Expect::Paints`] exists to report, and dropping it
//! here would turn "present but unpainted" into "absent" and lose the cause.

use crate::qa::{Check, Expect};

pub fn checks() -> Vec<Check> {
    vec![
        Check {
            id: "dialog-opens",
            group: "dialog",
            what: "the fork dialog opens when its control is pressed",
            open: Some("Home"),
            hover: None,
            click: Some("Fork "),
            press: true,
            subject: "Start fork",
            expect: Expect::Paints,
            panel_only: false,
        },
        Check {
            id: "dialog-cancel-dismisses",
            group: "dialog",
            what: "the fork dialog's Cancel actually dismisses it",
            open: Some("Home"),
            hover: None,
            // The trap: `AppModal` re-parented its root with
            // `document.body.append`, Blitz reallocated the slot, and the
            // painted dialog was no longer the node the handlers were bound
            // to. Hits landed on the copy, so two Cancels, `Start fork` and
            // Escape were all inert with no JS error, and 68 of the project
            // surface's 84 controls sat unreachable behind it.
            //
            // `Vanishes`, not `Absent`. Dismissing does not remove the
            // dialog from the tree: measured after Cancel, `Start fork` is
            // still there at `0x0 HIDDEN`. Absence was the wrong question,
            // and asking it reported a working Cancel as broken. What
            // distinguishes dismissed from trapped is the box: 1344x900
            // while open, nothing once closed.
            click: Some("Cancel"),
            press: true,
            subject: "Start fork",
            expect: Expect::Vanishes,
            panel_only: false,
        },
    ]
}

pub const PANEL_LEFT: f64 = 900.0;

