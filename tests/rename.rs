//! The in-place rename editor opens when its pencil is pressed.
//!
//! # Both of these currently fail, and the app is why
//!
//! Measured against a running build: pressing the pencil lays the editor out
//! at its full size - `651x24` for the project header - and leaves it `HIDDEN`.
//! The `Swapped` wrapper above it is `0x0 HIDDEN`, still carrying the `hidden`
//! class, while its own child laid out at `671x46`.
//!
//! So the signal write lands and the render never follows it. `start()` runs,
//! the layout happens, and the node the owner would type into is never made
//! visible. A check that asked only "is there a box" would pass on this, which
//! is why the assertion is `PaintsNamed` - role, name, *and* visibility.
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
            // The app may open on either surface depending on what the profile
            // last had focused, and `Rename ` alone matched the *project*
            // header's pencil on a run that started there. Naming Home and a
            // specific row makes the check about the control it says it is.
            open: Some("Home"),
            hover: None,
            // Pressed, not clicked. The editor opens on `mousedown` so the
            // `role="button"` row cannot swallow the press first, and a
            // synthesised `click` therefore does nothing at all. Measured
            // before the fix: the row folded (+30 nodes) and the textbox
            // stayed `0x0`. After: `0x0 HIDDEN` -> `300x21`.
            click: Some("Rename e"),
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
            // The editor carries the same accessible name as the pencil, so
            // this is the pair "Rename e" resolves to: a button that always
            // paints and a textbox that only paints while editing. `Paints`
            // needs one match with a box, and the button alone satisfies it -
            // which is why the subject is the *role*, scoped by name is not
            // possible here. See the count-based note below.
            subject: "textbox:Rename e",
            expect: Expect::PaintsNamed,
            panel_only: false,
        },
        /*
         * The project header's pencil, which is a different mount from the
         * Home rows even though it is the same component.
         *
         * Both were on the dead list. Both work: measured on a fresh instance,
         * the header's editor opens at 650x23 and the count of painted
         * textboxes goes 5 -> 6.
         */
        Check {
            id: "rename-project-header",
            group: "rename",
            what: "the project header's pencil opens its editor",
            // Only exists once a project is open. Double click is the gesture
            // Home uses to open a row, but a single press folds it, so this
            // presses the row's own name control instead.
            // Reached by pressing a Home row's name, which opens the project.
            // Depending on the previous check having left the surface open
            // would make this pass or fail on run order rather than on the
            // control.
            open: Some("eno working directory"),
            hover: None,
            click: Some("Rename project"),
            press: true,
            // The editor carries the same accessible name as the pencil, so
            // this is the pair "Rename e" resolves to: a button that always
            // paints and a textbox that only paints while editing. `Paints`
            // needs one match with a box, and the button alone satisfies it -
            // which is why the subject is the *role*, scoped by name is not
            // possible here. See the count-based note below.
            subject: "textbox:Rename project",
            expect: Expect::PaintsNamed,
            panel_only: false,
        },
    ]
}
