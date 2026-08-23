//! Icons resolve to artwork, not just to a box.

use crate::qa::{Check, Expect};

pub fn checks() -> Vec<Check> {
    vec![
        // ---- icons -----------------------------------------------------
        //
        // Asked of the box, not the tag. The semantic tree reports roles and
        // never element names, so "is there an `<svg>`" is unanswerable here
        // and looking for one returned zero against an app full of icons. What
        // is answerable, and what actually regressed, is whether the icon nodes
        // have a box: an icon whose artwork failed to resolve still lays out at
        // its `1em` and still reports its stroke, so geometry alone is not
        // enough either. `icon-art.test.ts` covers the artwork itself, which is
        // the half this cannot see.
        Check {
            id: "icons-paint",
            group: "icons",
            what: "icon nodes occupy a box on screen",
            open: Some("theta theta north indi"),
            hover: None,
            click: None,
            subject: "presentation",
            expect: Expect::Paints,
            press: false,
            panel_only: false,
        },
    ]
}

