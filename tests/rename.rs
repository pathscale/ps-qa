//! The in-place rename editor opens when its pencil is pressed.
//!
//! This shipped dead on 31 surfaces with a green unit suite either side of it,
//! and the failure is invisible to a DOM-only environment: the row around the
//! pencil is a `role="button"` that folds on `click`, the framework delegates
//! `click`, and the pencil's `stopPropagation` lost that race. jsdom has no
//! competing handler, so it never saw the conflict.
//! A dialog opens *and* can be dismissed.
//!
//! Two halves, in order, and the order matters: the checks run in sequence
//! against one instance, so the second inherits the dialog the first opened.
//! Splitting them is what makes a failure legible - "it never opened" and "it
//! opened and would not close" are different bugs, and the one that shipped
//! was the second. It trapped the window and put 68 of that surface's 84
//! controls out of reach behind one dialog.

use crate::qa::{Check, Expect};

pub fn checks() -> Vec<Check> {
    vec![
        Check {
            id: "rename-opens-editor",
            group: "rename",
            what: "pressing the pencil opens an editor the owner can type into",
            hover: None,
            // Pressed, not clicked. The editor opens on `mousedown` so the
            // `role="button"` row cannot swallow the press first, and a
            // synthesised `click` therefore does nothing at all. Measured
            // before the fix: the row folded (+30 nodes) and the textbox
            // stayed `0x0`. After: `0x0 HIDDEN` -> `300x21`.
            click: Some("Rename "),
            press: true,
            /*
             * `Grows`, counting textboxes that are actually on screen.
             *
             * Two weaker subjects were tried and both pass while the control
             * is dead, which is worse than no check at all:
             *
             * - By name (`Rename …`): `EditableTitle` gives the pencil and its
             *   editor the same accessible name, so the pencil's own box
             *   satisfies it whether or not the editor opens.
             * - `Paints` on `textbox`: the composer and the search field are
             *   already-painted textboxes, so *some* textbox always paints.
             *   Verified by mutation - reintroducing the bug left this green.
             *
             * What is actually true of the fix and false of the bug is that
             * pressing the pencil puts one *more* usable field on screen than
             * there was before. `Grows` counts only nodes that paint, so the
             * `0x0` editor sitting hidden beside every project name does not
             * inflate the baseline.
             *
             * Measured before the fix: pressing folded the row (+30 nodes) and
             * every editor `textbox` stayed `0x0`. After: `0x0` -> `300x21`.
             *
             * Run this against a *freshly launched* instance. It asserts a
             * delta, so an editor left open by an earlier press is already in
             * the baseline and the check reports `2 -> 2` on a working build.
             * `scripts/button-sweep.sh` restores the pristine profile for
             * exactly this reason.
             */
            subject: "textbox",
            expect: Expect::PaintsMore,
            panel_only: false,
        },
    ]
}

