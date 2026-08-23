//! Settings: the surface that had fourteen controls and no checks at all.
//!
//! It was the worst-covered surface in the application when the inventory was
//! first written: `cover settings` reported 14 buttons, 10 of them pressed and
//! asserted only as "something in the tree changed", 3 native file panels that
//! cannot be driven, and not one check asserting an outcome.
//!
//! # Why these count menu entries instead of naming one
//!
//! Every control here opens a dropdown, and **not one menu entry has an
//! accessible name**. Read straight off a live instance, every `menuitem` row
//! ends at `content=...` with nothing after it. So `PaintsNamed`, which is the
//! usual way to say "the thing this control opens appeared", cannot be written
//! for any of them: there is no name to match. That is tracked as its own
//! finding, because a menu a screen reader cannot announce is a menu nobody can
//! use without sight, and it is the same defect that leaves 70 buttons
//! unreachable by any check.
//!
//! # Why `PaintsMore` and not a count of the tree
//!
//! The menus are retained. Measured across four consecutive opens the tree grew
//! 32 -> 34 -> 38 -> 43 -> 49 menuitems and never shrank, because a closed menu
//! is laid out at `0x0` rather than removed. Counting tree membership therefore
//! rises whether or not the control being pressed did anything, and after a few
//! checks it can never fall.
//!
//! Counting only entries that *paint* is stable and falsifiable: measured
//! directly, 0 paint with every menu closed and 6 paint with one open. If a
//! dropdown stops opening, this goes red on the run that broke it.
//!
//! # `settings-agent-menu` fails, and the control is fine
//!
//! It reports `6 -> 2`: fewer entries painting after the press than before,
//! which is what pressing a control while *another* menu is open looks like.
//! The open menu is not left by a previous check - it fails the same way run
//! alone - it is opened by this check's own arrival step on the way to
//! Settings.
//!
//! Driven by hand from a clean surface the control is correct: 0 painting
//! before, 2 after. So the failure is the harness arriving dirty, which is the
//! first open issue in `issues.md`, and this is the sharpest reproduction of it
//! yet: one check, run by itself, on a freshly restored profile.
//!
//! Left failing on purpose. Deleting it would hide the arrival bug, and
//! weakening it to `Holds` would make a genuinely broken dropdown pass.
//!
//! # The four that are not here
//!
//! `Refresh`, `Stop` and `Re-check` act on the update channel and their outcome
//! is a network round trip, so they need a store or status assertion rather than
//! a geometry one. Three further controls open native macOS file panels, which
//! cannot be driven at all. Both are recorded in `issues.md` rather than
//! silently skipped.

use crate::qa::{Check, Expect};

pub fn checks() -> Vec<Check> {
    vec![
        Check {
            id: "settings-language-menu",
            group: "settings",
            what: "the language control opens a menu",
            open: Some("Settings"),
            hover: None,
            click: Some("Current language: English"),
            press: true,
            subject: "menuitem",
            expect: Expect::PaintsMore,
            panel_only: false,
        },
        Check {
            id: "settings-agent-menu",
            group: "settings",
            what: "the default agent control opens a menu",
            open: Some("Settings"),
            hover: None,
            click: Some("Default agent"),
            press: true,
            subject: "menuitem",
            expect: Expect::PaintsMore,
            panel_only: false,
        },
        Check {
            id: "settings-effort-menu",
            group: "settings",
            what: "the default effort control opens a menu",
            open: Some("Settings"),
            hover: None,
            click: Some("Default effort"),
            press: true,
            subject: "menuitem",
            expect: Expect::PaintsMore,
            panel_only: false,
        },
        Check {
            id: "settings-permission-menu",
            group: "settings",
            what: "the default permission control opens a menu",
            open: Some("Settings"),
            hover: None,
            click: Some("Default permission"),
            press: true,
            subject: "menuitem",
            expect: Expect::PaintsMore,
            panel_only: false,
        },
    ]
}
