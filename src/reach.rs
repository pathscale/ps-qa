//! Getting a control on screen before judging it.
//!
//! # Why this exists
//!
//! A sweep that plans from one snapshot of whatever surface the application
//! happened to open on, and drops any button whose box is `0x0`, measures a
//! fraction of the window and says nothing about the rest. Against a real
//! application that is not a detail: in the run that motivated this module,
//! 286 buttons were in the tree, 64 had a box, and 222 were quietly discarded.
//! None of the 222 were hidden. They were the per-row controls, on a surface
//! the sweep never visited.
//!
//! A skip is indistinguishable from a pass in the output, so a run that touched
//! a fifth of the window reported "every button acted". That is the whole bug.
//! Coverage was one screen deep and the report did not say so.
//!
//! # What this does instead
//!
//! Reaching is a first-class step with its own verdict. Before a control is
//! judged it is *brought into view*: its surface is opened, its section is
//! expanded, its row is hovered, and it is scrolled to. Only then is it clicked.
//! If none of that gives it a box it is counted as *unreachable* and printed,
//! rather than dropped, because a control the harness cannot reach is either a
//! real defect or a gap in this file, and both need to be visible.
//!
//! The rule the whole module turns on: **never silently skip.** Every button in
//! the tree ends in exactly one bucket, and the buckets are printed.

use std::collections::HashMap;

use blitz_control_protocol::SemanticNode;

/// A surface the sweep must visit, named by the control that opens it.
///
/// Defined by the application in its profile; this alias keeps the call sites
/// reading as they did when the list was hardcoded here.
pub use crate::app::SurfaceSpec as Surface;

/// Every top-level surface, in the order they are swept.
///
/// Read from the application's profile. The order is the application's to
/// choose and it matters: a surface that owns destructive row controls belongs
/// last, or visiting it first deletes the rows the other surfaces are reached
/// through. A harness cannot know which surface that is.
///
/// Every surface names an opener, including the one the app happens to launch
/// on. An empty opener meant "wherever we already are", which held only until
/// the first control that changed pane: the run pressed one button, navigated,
/// and counted the remaining 169 as vanished.
pub fn surfaces() -> &'static [crate::app::SurfaceSpec] {
    &profile().surfaces
}

/// Stands in for "the first document tab in the strip", resolved when the sweep
/// runs because a document's own name is user data and varies per profile.
pub const DYNAMIC_DOCUMENT: &str = "\u{0}dynamic-document";

/// A control that opens a document, either its tab or its row in a list.
///
/// Prefers a tab already in the strip, because activating one is a pane switch
/// rather than a load. A fresh profile may have no document tabs open at all,
/// so the fallback is a row in the root surface's list.
///
/// A row is recognised by the summary the list renders beside its name, which
/// the application states as `document_row_markers`. Matching the document's own
/// name is not possible: it is user data and differs per profile.
pub fn document_opener(nodes: &[SemanticNode]) -> Option<String> {
    let closes: Vec<String> = nodes
        .iter()
        .filter(|n| n.role == "button" && onscreen(n))
        .filter_map(|n| {
            profile()
                .close_prefixes
                .iter()
                .find_map(|prefix| n.name.strip_prefix(prefix.as_str()))
                .map(str::to_owned)
        })
        .collect();
    /*
     * A document tab, which is any doubled label that is not a permanent
     * surface.
     *
     * This used to filter on `!navigates(name)`, which was correct until
     * `navigates` was taught that document tabs are navigation - after that it
     * excluded every candidate and the project surface could never be opened.
     * The two need different questions: `navigates` asks "does pressing this
     * leave the surface I am sweeping", and this asks "is this the way in".
     */
    let tab = nodes
        .iter()
        .filter(|n| n.role == "button" && onscreen(n))
        .filter(|n| !profile().is_permanent(&n.name))
        .find(|n| {
            doubled(&n.name).is_some_and(|label| !profile().is_permanent(label))
                || closes
                    .iter()
                    .any(|subject| n.name == format!("{subject}{subject}"))
        });
    if let Some(tab) = tab {
        return Some(tab.name.clone());
    }
    /*
     * Otherwise a row in the root surface's list, which is what a person
     * clicks to open one. A fresh profile may have no document tabs in the
     * strip at all, so without this the surface is unreachable on exactly the
     * runs that matter.
     *
     * Preferring a document that has something in it: an empty one renders its
     * panes as a row of empty headers with every per-item control absent, so
     * the pane a person cares about most would be on screen with nothing to
     * press.
     */
    let rows = || {
        nodes
            .iter()
            .filter(|n| n.role == "button" && onscreen(n))
            .filter(|n| {
                let skip = |prefixes: &Vec<String>| {
                    prefixes.iter().any(|p| n.name.starts_with(p.as_str()))
                };
                !skip(&profile().close_prefixes) && !skip(&profile().row_action_prefixes)
            })
    };
    /*
     * Preferring a row whose summary carries a non-zero count.
     *
     * The first marker is treated as the one a count precedes, because a row
     * with something in it opens a populated pane and an empty one opens four
     * empty headers. If no marker parses a count this falls through to plain
     * marker matching, which is the honest behaviour for an application whose
     * rows carry no counts at all.
     */
    let markers = &profile().document_row_markers;
    let populated = markers.first().and_then(|first| {
        rows().find(|n| {
            n.name
                .split(first.as_str())
                .next()
                .and_then(|head| head.rsplit(')').next())
                .and_then(|count| count.trim().rsplit(' ').next())
                .and_then(|count| count.parse::<u32>().ok())
                .is_some_and(|open| open > 0)
        })
    });
    if let Some(row) = populated {
        return Some(row.name.clone());
    }
    rows()
        .find(|n| {
            markers
                .iter()
                .any(|marker| n.name.contains(marker.as_str()))
        })
        .map(|n| n.name.clone())
}

/// Whether a node is on screen well enough to click.
///
/// Both dimensions, because a control laid out at zero width is one no pointer
/// can land on even though the tree lists a box for it.
pub fn onscreen(node: &SemanticNode) -> bool {
    node.visible && node.bounds.is_some_and(|b| b[2] > 0.0 && b[3] > 0.0)
}

/// Whether pressing this leaves the surface, invalidating the rest of the plan.
///
/// The first repeatable run planned 173 buttons, pressed `Home` as the first of
/// them, and lost the other 169: they were all on the surface it had just
/// navigated away from. A control that changes surface has to be swept last, or
/// it takes the plan with it.
///
/// Matched on the tab-strip and nav entries by name. Deliberately a small,
/// explicit list rather than a guess about which names look like navigation:
/// over-matching here silently drops controls from the sweep, which is the
/// failure this whole module exists to end.
pub fn navigates(name: &str) -> bool {
    // The application's own permanent tabs. `is_permanent` accepts the doubled
    // form the strip renders ("HomeHome"), which an exact match would miss and a
    // `contains` would over-match into "Close Home".
    if profile().is_permanent(name) {
        return true;
    }
    /*
     * A tab in the strip, whose label the strip doubles.
     *
     * These are the controls that cost the sweep Home: clicking `ee` switched
     * to that project's pane, and Home's 160 remaining controls went to
     * `visible=false` while staying in the retained DOM. They were reported as
     * vanished when the sweep had simply walked off the surface.
     *
     * A project tab is swept as the opener of the project surface, so skipping
     * it here loses no coverage.
     */
    doubled(name).is_some()
}

/// The single label behind a doubled tab-strip name, if it is one.
///
/// `"ee"` -> `"e"`, `"HomeHome"` -> `"Home"`. An odd length or a mismatched
/// half is not a tab.
fn doubled(name: &str) -> Option<&str> {
    if name.is_empty() || !name.len().is_multiple_of(2) {
        return None;
    }
    let (left, right) = name.split_at(name.len() / 2);
    (left == right && !left.trim().is_empty()).then_some(left)
}

/// Whether the window is still showing the surface a plan was made against.
///
/// Each surface is recognised by a control only it renders. That is enough to
/// answer the one question the sweep needs - "did the last click take us
/// somewhere else" - without a route or a title to read, neither of which the
/// semantic tree exposes.
pub fn on_surface(nodes: &[SemanticNode], surface: &Surface) -> bool {
    let Some(marker) = surface.marker.as_deref() else {
        return true;
    };
    nodes.iter().any(|n| onscreen(n) && n.name.contains(marker))
}

/// The controls that belong to the surface in front, by ancestry.
///
/// # Why not position, and not visibility
///
/// Both were tried against the running app and both are wrong. A retained Home
/// sits *behind* an open project pane and its rows keep real boxes in the same
/// horizontal band: `Items1` in the panel measured x=953 and Home's
/// a list header in the same region, so a coordinate cut cannot separate them. Worse, the
/// retained rows still report `visible` with a non-zero box, so filtering on
/// visibility keeps every one of them too.
///
/// The consequence was not a small error. Home's ~160 row controls were swept
/// as though they were the project panel's, the panel's own controls were
/// crowded out of the plan, and users - who reports the side panels as
/// where most problems are - was reading coverage numbers for the wrong
/// surface.
///
/// Ancestry is the one thing that does separate them: a pane is a subtree, and
/// the marker control that identifies a surface lives inside it. Walking up
/// from the marker to the pane root and then taking that root's descendants
/// gives exactly the controls a person is looking at.
pub fn on_surface_subtree(nodes: &[SemanticNode], surface: &Surface) -> Vec<u64> {
    let Some(marker) = surface.marker.as_deref() else {
        return nodes.iter().map(|n| n.id).collect();
    };
    let by_id: HashMap<u64, &SemanticNode> = nodes.iter().map(|n| (n.id, n)).collect();
    let Some(anchor) = nodes
        .iter()
        .find(|n| onscreen(n) && n.name.contains(marker))
    else {
        return Vec::new();
    };

    /*
     * Up a fixed number of levels, not to the document root.
     *
     * Walking all the way up lands on the window, whose subtree is every
     * surface at once - which is the situation this exists to end. Eight is
     * deep enough to clear a control's own chrome and reach the pane, and
     * shallow enough not to swallow its neighbour; it is the same depth
     * an in-place editor's notes use for "an input that is merely hidden still
     * walks eight levels to the window root".
     */
    /*
     * The shallowest ancestor that holds most of what is on screen.
     *
     * A fixed climb cannot work for every surface: eight levels from Home's
     * sort control landed above its list and returned nothing at all, while the
     * same depth from a project's `Send` was right. So the depth is chosen by
     * measurement - climb one level at a time and keep the first ancestor whose
     * subtree covers a majority of the on-screen controls. That is the pane,
     * whichever surface it belongs to, and it stops before the window root,
     * whose subtree is every surface at once.
     */
    let onscreen_total = nodes
        .iter()
        .filter(|n| n.role == "button" && onscreen(n))
        .count();
    /*
     * Descended from the root, not climbed from every node.
     *
     * The per-node climb needed a hop limit to stay bounded, and any limit is
     * wrong: this tree runs to 8317 nodes and a project row sits deeper than
     * thirty-two ancestors, so the cap silently dropped exactly the controls
     * the sweep exists to press and Home reported zero buttons. Walking down
     * from the root visits each node once and has no depth to guess at.
     */
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    for node in nodes {
        if let Some(parent) = node.parent {
            children.entry(parent).or_default().push(node.id);
        }
    }
    let subtree_of = |root: u64| -> Vec<u64> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            out.push(id);
            if let Some(kids) = children.get(&id) {
                stack.extend(kids.iter().copied());
            }
        }
        out
    };

    /*
     * The ancestor holding the most on-screen controls, chosen over the whole
     * climb rather than at the first one to clear a threshold.
     *
     * A majority test looks reasonable and fails exactly when it matters: after
     * a full run the window holds several retained panes, no single ancestor
     * reaches half the on-screen buttons, the loop exhausts, and it returns
     * whatever the last ancestor happened to be. Home reported zero buttons
     * that way while sweeping it alone found 145 - a coverage hole that only
     * appeared in the run that was supposed to cover everything.
     *
     * Taking the maximum has no threshold to be wrong about. The climb stops
     * short of the document root, whose subtree is every surface at once.
     */
    let mut cursor = anchor.id;
    let mut best: Vec<u64> = Vec::new();
    let mut best_covered = 0usize;
    for _ in 0..12 {
        let Some(parent) = by_id.get(&cursor).and_then(|n| n.parent) else {
            break;
        };
        cursor = parent;
        let kept = subtree_of(cursor);
        let covered = kept
            .iter()
            .filter(|id| {
                by_id
                    .get(id)
                    .is_some_and(|n| n.role == "button" && onscreen(n))
            })
            .count();
        // Everything on screen means this is the root, not a pane.
        if onscreen_total > 0 && covered >= onscreen_total {
            break;
        }
        if covered > best_covered {
            best_covered = covered;
            best = kept;
        }
    }
    best
}

/// Whether pressing this hands control to the operating system.
///
/// A native file chooser is not part of the webview: it is a modal the harness
/// cannot see in the semantic tree, cannot dismiss with a click, and which
/// takes the user's screen until a person closes it. A sweep that presses one
/// stops being unattended, and a user reported exactly that - "the open file
/// dialog is stuck open on the GUI" - mid-run.
///
/// These are skipped rather than judged, and counted in their own bucket so the
/// report never implies they passed.
pub fn requires_manual_release_check(name: &str) -> bool {
    /*
     * Only the controls that hand the screen to macOS.
     *
     * This list used to carry "Choose", "Browse" and "Open folder" as well,
     * which is how it grew from "skip the OS file chooser" into a general
     * posture of not opening things. That posture is what let one dialog
     * ship with a dead Cancel: the sweep never opened an in-app modal, so it
     * never asked whether it could get back out, and a user found a trap the
     * harness had reported as a clean run.
     *
     * An in-app dialog is a surface like any other and gets swept. Only a
     * native chooser is exempt, because it is not in the webview at all: the
     * tree cannot see it, no click can dismiss it, and it takes the user's
     * screen until a person closes it.
     */
    /*
     * The only exception, and it is documented rather than silent.
     *
     * Everything else is pressed, including in-app modals: the old list had
     * grown into a general posture of not opening things, which is how a fork
     * dialog shipped with a Cancel that does nothing - no run ever opened it.
     *
     * A macOS file chooser is genuinely outside what this harness can drive.
     * It is not in the webview, so the semantic tree cannot see it and no
     * synthesised click can reach it; Escape through the control protocol goes
     * to the window underneath, and driving it through System Events needs
     * assistive access this process does not have. Pressing one leaves a panel
     * on the user's screen until a person closes it, which happened twice
     * during this audit.
     *
     * These are counted in the `native` bucket and printed, so the report says
     * how many controls were not exercised and why. They need a person:
     * `scripts/button-sweep.sh` documents the manual pass.
     */
    profile()
        .manual_controls
        .iter()
        .any(|exception| name.starts_with(exception.label.as_str()))
}

/// Whether the window is showing a modal that has to be dismissed to continue.
///
/// A modal is the one thing a sweep cannot treat as ordinary: every control
/// behind it is unreachable until it closes, so a dialog that will not dismiss
/// does not fail one button, it ends the run. The check is therefore not "did
/// this button act" but "can I still get out of here".
pub fn modal_open(nodes: &[SemanticNode]) -> bool {
    nodes.iter().any(|node| {
        onscreen(node) && node.role == "button" && (node.name == "Cancel" || node.name == "Dismiss")
    })
}

/// The controls that would dismiss the modal in front, best first.
///
/// `Cancel` before `Close`, because a dialog often renders both an × in its
/// header and a `Cancel` in its footer and either should work; trying the named
/// one first keeps the report readable when neither does.
pub fn dismissers(nodes: &[SemanticNode]) -> Vec<(u64, String)> {
    let mut found: Vec<(u64, String)> = nodes
        .iter()
        .filter(|n| n.role == "button" && onscreen(n))
        .filter(|n| matches!(n.name.as_str(), "Cancel" | "Dismiss" | "Close"))
        .map(|n| (n.id, n.name.clone()))
        .collect();
    found.sort_by_key(|(_, name)| match name.as_str() {
        "Cancel" => 0,
        "Dismiss" => 1,
        _ => 2,
    });
    found
}

/// Whether this restarts the app or reopens onboarding.
///
/// `Welcome Tutorial` and `Restart` put a setup flow in front of everything and
/// open tabs of their own. After a Settings sweep pressed them the window was
/// left with `Close setup` in the strip and Analytics could not be opened at
/// all - three clicks, no navigation - while Home still worked. That is the
/// sweep breaking its own run, not a defect in the button.
///
/// Left to a person, because "does the tutorial replay" is a question about a
/// flow rather than about one control's promise.
pub fn restarts_the_app(name: &str) -> bool {
    const DISRUPTIVE: &[&str] = &["Welcome Tutorial", "Restart", "Reset all", "Sign out"];
    DISRUPTIVE.iter().any(|entry| name.starts_with(entry))
}

/// Whether this closes a surface the sweep still has to stand on.
///
/// `Close Settings` retires the tab that every later surface is reached
/// through, so pressing it early cost the run its own subject: Home planned 20
/// controls against a window that had 196 on screen, because the sweep was no
/// longer where it thought it was.
///
/// The tab is still exercised - `Close` on a project tab is swept - but the
/// three permanent surfaces keep theirs.
pub fn closes_a_surface(name: &str) -> bool {
    /*
     * Every `Close` in the strip, not a fixed list of three.
     *
     * A project tab's close was left in the sweep on the grounds that it is an
     * ordinary control - but closing a project tab falls the window back to
     * Home, retires the pane later surfaces are reached through, and in a full
     * run left Home reporting zero buttons where sweeping it alone finds 145.
     * The close controls are worth pressing; they are not worth pressing in the
     * middle of a plan that stands on what they remove.
     */
    profile()
        .close_prefixes
        .iter()
        .any(|prefix| name.starts_with(prefix.as_str()))
}

/// The subtree of the dialog that owns this dismiss control.
///
/// A modal does not remove the surface behind it: that surface stays in the
/// tree, `visible` and sized, the same way a retained pane does. So "everything
/// on screen" is not "everything in the dialog", and sweeping the former made
/// one dialog's pass press `HomeHome`, `Settings` and `Attach files` -
/// controls that are not in the dialog at all, one of which raises a macOS
/// panel onto the user's screen.
///
/// Found by climbing from the dismiss control until the subtree stops growing
/// quickly, which is the dialog's own container: a modal is a small, self
/// contained box next to a large surface, so the first ancestor holding more
/// than a handful of controls is already too big.
pub fn enclosing_dialog(nodes: &[SemanticNode], dismiss_id: u64) -> Vec<u64> {
    let by_id: HashMap<u64, &SemanticNode> = nodes.iter().map(|n| (n.id, n)).collect();
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    for node in nodes {
        if let Some(parent) = node.parent {
            children.entry(parent).or_default().push(node.id);
        }
    }
    let subtree_of = |root: u64| -> Vec<u64> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            out.push(id);
            if let Some(kids) = children.get(&id) {
                stack.extend(kids.iter().copied());
            }
        }
        out
    };

    let mut cursor = dismiss_id;
    let mut best = vec![dismiss_id];
    for _ in 0..8 {
        let Some(parent) = by_id.get(&cursor).and_then(|n| n.parent) else {
            break;
        };
        cursor = parent;
        let kept = subtree_of(cursor);
        let buttons = kept
            .iter()
            .filter(|id| by_id.get(id).is_some_and(|n| n.role == "button"))
            .count();
        /*
         * A dialog holds a handful of controls. Past that this is the pane
         * behind it, and taking that would sweep the whole surface again from
         * inside the modal.
         */
        if buttons > 12 {
            break;
        }
        best = kept;
    }
    best
}

/// Whether pressing this is likely to raise a macOS file chooser.
///
/// Not an exclusion - these are pressed like everything else - but a cue to
/// send Escape straight afterwards. A native panel is not in the webview: the
/// semantic tree cannot see it, no synthesised click can dismiss it, and it
/// holds the user's screen until a person closes it. Testing the control and
/// then getting out of the way is what a person does, and it is the only shape
/// that satisfies "test everything" without leaving the app wedged.
pub fn may_open_native_chooser(name: &str) -> bool {
    const NATIVE: &[&str] = &[
        "Attach files",
        "Add dir",
        "Select backup file",
        "Choose",
        "Browse",
        "Open folder",
        "Import",
        "Export",
    ];
    NATIVE.iter().any(|entry| name.starts_with(entry))
}

/// Whether this is a panel section header, which folds the rows beneath it.
///
/// The panel's headers are named for their contents and count - `Items1`,
/// `Items0`, `Log22` - not "Collapse Items", so a
/// name-prefix rule for disclosures misses every one of them. Pressing `Items1`
/// folds the section, and with it `Fork <item> into a fresh chat`, `Change the
/// status of ...`, `Edit the description for ...` and `New item`.
///
/// That is precisely how one dialog escaped the audit: the sweep folded
/// the Items section a dozen controls before it reached the rows, so every
/// per-item control read as vanished and the dialog was never opened.
pub fn folds_a_section(name: &str) -> bool {
    /*
     * Read from the application's own profile, falling back to nothing.
     *
     * These are six section names from one product, and a harness has no
     * business knowing them. An application states its own in `ps-qa.ron`; one
     * that ships no profile gets an empty list, which means the sweep presses
     * sections in plan order rather than last. That is a worse sweep, not a
     * wrong one, and it beats hunting for another product's headers.
     *
     * Read once: this is called per button per surface, and a file read per
     * call would dominate the sweep.
     */
    profile().folds_a_section(name)
}

/// The application profile, read once.
///
/// This is called per button per surface, so a file read per call would
/// dominate the sweep.
pub fn profile() -> &'static crate::app::AppProfile {
    static PROFILE: std::sync::OnceLock<crate::app::AppProfile> = std::sync::OnceLock::new();
    PROFILE.get_or_init(|| match crate::app::AppProfile::load(None) {
        Ok(profile) => profile,
        // No fallback profile, and no continuing without one.
        //
        // A missing or unparseable profile is a typo or a wrong working
        // directory, never a decision to run against a description of some
        // other application. Substituting an empty one is how a sweep reports
        // "0 sections opened" against an application with six of them, and
        // nobody notices for an afternoon: every number after that point is
        // measured against something that does not exist, which is worse than
        // no number at all.
        //
        // `main` validates the profile before it drives anything, so reaching
        // here means a unit test asked for the profile without one on disk.
        // Those tests cover the half of this module that is about how a tab
        // strip renders rather than about any one application, so an empty
        // profile is the honest answer for them and unreachable outside them.
        #[cfg(test)]
        Err(_) => crate::app::AppProfile::default(),
        #[cfg(not(test))]
        Err(error) => panic!("no application profile: {error}"),
    })
}

/// The disclosure controls that must be opened before a sweep of this surface.
///
/// Collapsed sections are the second-largest source of unreached controls after
/// the wrong surface: `Items` alone hides a row of controls per item, and the
/// QA profile carries twenty-three of them.
pub fn expanders(nodes: &[SemanticNode]) -> Vec<(u64, String)> {
    nodes
        .iter()
        .filter(|node| node.role == "button" && onscreen(node))
        .filter(|node| node.name.to_lowercase().starts_with("expand "))
        .map(|node| (node.id, node.name.clone()))
        .collect()
}

/// Rows worth hovering, as the point at the middle of each.
///
/// Row actions do not exist until `pointerenter`, so a sweep that never moves
/// the pointer cannot see them at all. Hovering the row rather than the control
/// is the only order that works: the control is not in the tree to be aimed at
/// until the row it lives in is hovered.
/// Only rows whose middle is inside the window: a transcript keeps hundreds of
/// rows at negative coordinates, and moving the pointer to y=-9726 reveals
/// nothing while costing a round trip each.
pub fn hover_points(
    nodes: &[SemanticNode],
    row_role: &str,
    window: (f64, f64),
) -> Vec<(u64, String)> {
    nodes
        .iter()
        .filter(|node| node.role == row_role && onscreen(node))
        .filter_map(|node| {
            let b = node.bounds?;
            let (x, y) = (b[0] + b[2] / 2.0, b[1] + b[3] / 2.0);
            (y >= window.0 && y <= window.1 && x >= 0.0).then(|| (node.id, format!("{x},{y}")))
        })
        .collect()
}

/// How every button in the tree was accounted for.
///
/// Printed at the end of a run so a coverage regression is visible as a number
/// rather than as silence. `swept + unreachable + hidden` must equal the button
/// count in the tree; if it does not, this file has a hole in it.
#[derive(Debug, Default, Clone, Copy)]
pub struct Coverage {
    pub in_tree: usize,
    pub swept: usize,
    pub unreachable: usize,
    pub hidden: usize,
    /// Planned, then gone by the time its turn came.
    ///
    /// Not a fault and not a skip: closing one tab removes its neighbours'
    /// close buttons, so a working control legitimately retires others. It gets
    /// its own bucket so it cannot be confused with a control that was never
    /// tried.
    pub vanished: usize,
    /// Leaves the surface, so it is exercised as an opener instead.
    pub navigation: usize,
    /// Hands the screen to a native modal, so it is never pressed unattended.
    pub manual: usize,
    /// Left unreachable behind a dialog that would not dismiss.
    ///
    /// Its own bucket because it is neither a pass nor a skip: these controls
    /// were planned, are on the surface, and could not be reached because one
    /// bug upstream of them traps the window. Counting them as anything else
    /// hides the blast radius of that bug.
    pub blocked: usize,
    /// Swept, but not present when `in_tree` was counted.
    ///
    /// A dialog's own controls do not exist until it opens, and a row's actions
    /// do not exist until it is hovered, so both are pressed without ever having
    /// been in the snapshot the total came from. Charged to `swept`, they pushed
    /// the buckets past the total and the consistency check reported a *negative*
    /// `UNACCOUNTED` - a surplus - which read as "more than covered" when the
    /// real meaning was that the denominator was wrong.
    ///
    /// Separate from `swept` so the surplus cannot hide a genuine gap: a run can
    /// now be short of coverage and full of dialogs at the same time and still
    /// report both.
    pub revealed: usize,
}

impl Coverage {
    /// Every button ended in a bucket.
    pub fn accounted(&self) -> bool {
        self.bucketed() == self.total()
    }

    /// Everything a run was responsible for: the surface's own buttons plus the
    /// ones that only came into existence once it started pressing things.
    pub fn total(&self) -> usize {
        self.in_tree + self.revealed
    }

    fn bucketed(&self) -> usize {
        self.swept
            + self.unreachable
            + self.hidden
            + self.vanished
            + self.navigation
            + self.manual
            + self.blocked
            + self.revealed
    }

    pub fn line(&self) -> String {
        format!(
            "{} buttons{}: {} swept, {} unreachable, {} hidden, {} vanished, {} nav, {} manual, {} blocked{}",
            self.total(),
            if self.revealed > 0 {
                format!(" ({} on open, {} revealed)", self.in_tree, self.revealed)
            } else {
                String::new()
            },
            self.swept,
            self.unreachable,
            self.hidden,
            self.vanished,
            self.navigation,
            self.manual,
            self.blocked,
            if self.accounted() {
                String::new()
            } else {
                format!(
                    " (UNACCOUNTED {})",
                    self.total() as i64 - self.bucketed() as i64
                )
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u64, role: &str, name: &str, bounds: Option<[f64; 4]>) -> SemanticNode {
        SemanticNode {
            id,
            parent: None,
            role: role.to_owned(),
            name: name.to_owned(),
            value: None,
            enabled: true,
            visible: true,
            selected: false,
            bounds,
        }
    }

    #[test]
    fn a_zero_box_control_is_not_onscreen() {
        // The exact shape of the 222 discarded controls: in the tree, not
        // hidden, no box.
        assert!(!onscreen(&node(1, "button", "Row action", Some([0.0; 4]))));
        assert!(onscreen(&node(
            2,
            "button",
            "Row action",
            Some([10.0, 10.0, 20.0, 20.0])
        )));
    }

    #[test]
    fn only_onscreen_expanders_are_offered() {
        let nodes = vec![
            node(1, "button", "Expand Items", Some([0.0, 0.0, 20.0, 20.0])),
            node(2, "button", "Expand Hidden", Some([0.0; 4])),
            node(
                3,
                "button",
                "Collapse Running",
                Some([0.0, 0.0, 20.0, 20.0]),
            ),
        ];
        let found = expanders(&nodes);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, "Expand Items");
    }

    #[test]
    fn navigation_is_recognised_without_swallowing_its_neighbours() {
        /*
         * The half of `navigates` that needs no profile.
         *
         * A document tab is recognised by its doubled label, which is a fact
         * about how a tab strip renders rather than about any one product's
         * surfaces. The permanent-tab half is the application's to state, and
         * is covered by `a_permanent_surface_is_recognised_doubled` in `app`.
         *
         * This test used to assert `navigates("Home")` directly, which was this
         * module knowing one application's tab names.
         */
        // Project tabs are doubled, and clicking one leaves the surface: this
        // is what cost Home 160 controls in a run.
        assert!(navigates("ee"));
        assert!(navigates("delta/east/cobaltdelta/east/cobalt"));
        // A `contains` would catch these, and dropping a Close from the sweep
        // is the silent skip this module exists to end.
        assert!(!navigates("Close Home"));
        assert!(!navigates("Add dir"));
        assert!(!navigates("Rename project"));
        // Not every even-length name is a doubled label.
        assert!(!navigates("Send"));
        assert!(!navigates("Copy"));
    }

    #[test]
    fn a_permanent_tab_navigates_when_the_profile_names_it() {
        // The mechanism, without hardcoding a product: whatever the application
        // calls its permanent tabs, both the plain and doubled forms navigate.
        let profile = crate::app::AppProfile {
            permanent_surfaces: vec!["Dashboard".to_owned()],
            ..Default::default()
        };
        assert!(profile.is_permanent("Dashboard"));
        assert!(profile.is_permanent("DashboardDashboard"));
        assert!(!profile.is_permanent("Close Dashboard"));
    }

    #[test]
    fn only_the_documented_file_panels_are_exempt() {
        /*
         * The exemption is exactly the controls the application named, matched
         * by prefix, and every entry says what it raises.
         *
         * Two failure modes this guards, both of which have happened:
         *
         * - Too broad. An exemption list that grows into a general posture of
         *   not opening things is how a dialog ships with a Cancel that does
         *   nothing: no run ever opened it. In-app modals must still be swept.
         * - Named wrong. An entry carrying the *row's* label rather than the
         *   *control's* matches nothing, and the sweep raises a panel it cannot
         *   dismiss while the list claims it is exempt. That failure is silent:
         *   the report shows the control as swept.
         */
        let profile = crate::app::AppProfile {
            manual_controls: vec![
                crate::app::ManualControl {
                    label: "Attach files".to_owned(),
                    command: "choose_attachments".to_owned(),
                },
                crate::app::ManualControl {
                    label: "Open http".to_owned(),
                    command: "openExternal".to_owned(),
                },
            ],
            ..Default::default()
        };
        let exempt = |name: &str| {
            profile
                .manual_controls
                .iter()
                .any(|e| name.starts_with(e.label.as_str()))
        };
        assert!(exempt("Attach files"));
        // A prefix, so a label that carries its subject still matches.
        assert!(exempt("Open https://example.invalid/pull/1"));
        // Ordinary controls are still swept.
        assert!(!exempt("Send"));
        assert!(!exempt("Cancel"));
        assert!(!exempt("New item"));

        // Every exception names what it opens, so the printed worklist is
        // actionable rather than a list of bare labels.
        assert!(
            profile
                .manual_controls
                .iter()
                .all(|e| !e.command.is_empty())
        );

        // An application that names none exempts nothing.
        assert!(crate::app::AppProfile::default().manual_controls.is_empty());
    }

    #[test]
    fn a_modal_is_recognised_and_offers_its_dismissers() {
        let dialog = vec![
            node(1, "button", "Cancel", Some([0.0, 0.0, 20.0, 20.0])),
            node(2, "button", "Fork", Some([0.0, 0.0, 20.0, 20.0])),
            node(3, "button", "Close", Some([0.0, 0.0, 20.0, 20.0])),
        ];
        assert!(modal_open(&dialog));
        // Cancel first: a dialog can render an x in its header and a
        // Cancel in its footer, and the named one reads better in a report.
        let ways_out = dismissers(&dialog);
        assert_eq!(ways_out[0].1, "Cancel");
        assert_eq!(ways_out.len(), 2);

        let ordinary = vec![node(1, "button", "Send", Some([0.0, 0.0, 20.0, 20.0]))];
        assert!(!modal_open(&ordinary));
    }

    #[test]
    fn onboarding_and_restart_are_left_alone() {
        // These left the window with `Close setup` in the strip and Analytics
        // unreachable for the rest of the run.
        assert!(restarts_the_app("Welcome Tutorial"));
        assert!(restarts_the_app("Restart"));
        // Ordinary Settings controls are still swept.
        assert!(!restarts_the_app("Re-check"));
        assert!(!restarts_the_app("Refresh"));
        assert!(!restarts_the_app("Default model"));
    }

    #[test]
    fn surface_tabs_are_not_closed_out_from_under_the_sweep() {
        /*
         * The prefixes come from the application, so this asserts the matching
         * rule against a made-up vocabulary rather than one product's labels.
         *
         * A tab's close counts even for a document tab, which it did not used
         * to. It is still pressed - the caller defers these to the end of the
         * plan rather than skipping them - but closing any tab retires a pane
         * the rest of the plan stands on. Exempting document tabs is what left
         * the root surface reporting zero buttons in a full run.
         */
        let profile = crate::app::AppProfile {
            close_prefixes: vec!["Dismiss ".to_owned()],
            ..Default::default()
        };
        let closes = |name: &str| {
            profile
                .close_prefixes
                .iter()
                .any(|prefix| name.starts_with(prefix.as_str()))
        };
        assert!(closes("Dismiss Preferences"));
        assert!(closes("Dismiss some/document/name"));
        // Not a close at all.
        assert!(!closes("Collapse Recent"));
        assert!(!closes("Rename thing"));
        // And an application that names no close prefix has none.
        let bare = crate::app::AppProfile::default();
        assert!(bare.close_prefixes.is_empty());
    }

    #[test]
    fn rows_off_the_top_of_a_transcript_are_not_hovered() {
        // The first run moved the pointer to y=-9726 eight times over. A row
        // above the window reveals nothing and costs a round trip.
        let nodes = vec![
            node(
                1,
                "listitem",
                "visible row",
                Some([10.0, 100.0, 200.0, 40.0]),
            ),
            node(
                2,
                "listitem",
                "scrolled off",
                Some([10.0, -9726.0, 200.0, 40.0]),
            ),
            node(
                3,
                "listitem",
                "below the fold",
                Some([10.0, 5000.0, 200.0, 40.0]),
            ),
        ];
        let points = hover_points(&nodes, "listitem", (0.0, 900.0));
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].0, 1);
    }

    #[test]
    fn coverage_reports_an_unaccounted_gap() {
        // Every bucket carries at least one control, so a bucket dropped from
        // `bucketed()` shows up here as an unaccounted gap rather than passing
        // on a zero that proves nothing. One dialog put real controls in
        // `blocked`, which is why it counts.
        let full = Coverage {
            in_tree: 10,
            swept: 3,
            unreachable: 2,
            hidden: 1,
            vanished: 1,
            navigation: 1,
            manual: 1,
            blocked: 1,
            revealed: 0,
        };
        assert!(full.accounted());
        assert!(!full.line().contains("UNACCOUNTED"));

        let leaky = Coverage {
            in_tree: 10,
            swept: 6,
            ..Default::default()
        };
        assert!(!leaky.accounted());
        assert!(leaky.line().contains("UNACCOUNTED 4"));
    }

    #[test]
    fn a_dialogs_own_controls_do_not_overflow_the_total() {
        /*
         * The regression: `cover home` reported `UNACCOUNTED -47`.
         *
         * A dialog's buttons do not exist when the surface is counted, so
         * pressing them used to increment `swept` against a total that never
         * included them. The buckets overran the denominator and the check went
         * negative - reporting a surplus, which reads as "better than covered"
         * when it actually means the number is meaningless.
         */
        let with_a_dialog = Coverage {
            in_tree: 10,
            swept: 10,
            revealed: 4,
            ..Default::default()
        };
        assert!(
            with_a_dialog.accounted(),
            "revealed controls extend the total: {}",
            with_a_dialog.line()
        );
        assert_eq!(with_a_dialog.total(), 14);
        assert!(with_a_dialog.line().contains("(10 on open, 4 revealed)"));
    }

    #[test]
    fn a_surplus_cannot_hide_a_real_gap() {
        // Ten on the surface, four of them never reached, plus four dialog
        // controls pressed. The old accounting cancelled one against the other
        // and reported a clean run; the gap has to survive the dialog.
        let both = Coverage {
            in_tree: 10,
            swept: 6,
            revealed: 4,
            ..Default::default()
        };
        assert!(!both.accounted());
        assert!(both.line().contains("UNACCOUNTED 4"), "{}", both.line());
    }
}
