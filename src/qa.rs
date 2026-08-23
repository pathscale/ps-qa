//! What a check is, and what its verdict means.
//!
//! # Why this exists
//!
//! A DOM-only test environment answers questions about a tree the user never
//! sees. It has no compositor, no hit-testing and no layout, so it reports a
//! node as fine while the screen shows nothing. A green suite over jsdom says
//! the code is self-consistent, not that the interface works.
//!
//! Two failure classes make the point, and both have shipped past a green unit
//! suite in a real application:
//!
//! - **Artwork that lays out and does not draw.** The semantic tree reports a
//!   node whether or not a pixel was painted, and a node can have a correct box
//!   and a correct stroke colour while drawing nothing at all. A renderer that
//!   parses each inline `<svg>` from that element's markup alone will resolve a
//!   reference into a shared sprite to nothing. jsdom cannot see this: its
//!   `<svg>` is a well-behaved object that never goes near a rasteriser.
//! - **Controls that only exist while hovered.** A test that never moves a
//!   pointer cannot see them at all.
//!
//! # What a check is
//!
//! A [`Check`] is a precondition, an action, and an assertion about the state
//! after it, all expressed against the running application. Every part is
//! observed rather than assumed, and [`Paints`](Expect::Paints) fails if the
//! element is in the tree with no box - which is the thing the semantic tree
//! alone will not tell you.
//!
//! Checks are data, and they belong to the application under test rather than
//! to this crate. See [`checks`] for where they are read from.
//!
//! ```sh
//! ps-qa qa           # every check
//! ps-qa qa icons     # one group
//! ```

use blitz_control_protocol::SemanticNode;
use std::collections::HashMap;

/// What a single check asserts once its action has run.
///
/// The full vocabulary is kept whether or not a check currently uses every
/// variant. These are the choices available when writing one, documented with
/// the failure each is right for, and a variant deleted for being momentarily
/// unused is a distinction the next person has to rediscover. `Grows` went
/// unconstructed the moment one check was strengthened to `PaintsMore`; it is
/// still the correct assertion for anything that mounts without painting.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub enum Expect {
    /// The named node exists, is visible, and has a non-zero box.
    ///
    /// Non-zero is the part that matters. A node with a box of `0x0` is in the
    /// tree and on no screen, which is how a broken control passes a test that
    /// only asked whether it existed.
    Paints,
    /// No node matching the name exists.
    ///
    /// The assertion for a control that must *not* be reachable, and for
    /// checking that a destructive action did not fire.
    Absent,
    /// The node may remain in the tree, but nothing matching it is on screen.
    ///
    /// The inverse of [`Paints`](Expect::Paints), and the right question for
    /// anything that closes. A dismissed dialog is not removed: measured after
    /// a dialog's Cancel, the dialog's own content is still in the tree at
    /// `0x0 HIDDEN`. Asking for absence there reports a working control as
    /// broken, while asking for a box distinguishes dismissed from trapped -
    /// full size while open, nothing once closed.
    Vanishes,
    /// The count of matching nodes changed in the given direction.
    Grows,
    /// More matching nodes are *on screen* than before.
    ///
    /// [`Grows`](Expect::Grows) counts tree membership, which is the wrong
    /// question for a control that reveals something already mounted. The
    /// rename editor is built for the life of its component and merely hidden,
    /// so the node count does not move when it opens - only its box does.
    ///
    /// Counting painted nodes is also what makes the check falsifiable. A
    /// `Paints` assertion on `textbox` stays green while the pencil is dead,
    /// because the composer and the search field are textboxes that always
    /// paint; verified by reintroducing the bug and watching it pass.
    PaintsMore,
    /// The count of matching nodes did not change.
    Holds,
    /// A node matching *both* a name and a role paints.
    ///
    /// The precise form of [`Paints`](Expect::Paints), for the common case
    /// where a control and the thing it opens share an accessible name.
    /// An in-place rename control does exactly that: `Rename <subject>`
    /// resolves to a button that always paints *and* a textbox that paints only
    /// while editing, so a name-only `Paints` is satisfied by the pencil
    /// whether or not the editor ever opens.
    ///
    /// A count-based assertion is not the answer either. `PaintsMore` over
    /// every `textbox` in the tree is fragile to whatever else happens to be
    /// on screen - a composer, a search field, an editor left open by an
    /// earlier check - and reported `2 -> 2` on a run where the editor
    /// demonstrably opened. Asking about one node by name and role is the
    /// question the check actually means.
    ///
    /// Written as `role:name`, e.g. `textbox:Rename thing`.
    ///
    /// Judged on geometry, not on the tree's `visible` flag, because the two
    /// disagree. Measured on one node at one instant: `paint` reported the
    /// rename editor `300.0x21.1 at 87,236 fg=#b0b5b9ff opacity=1.00 Visible`
    /// while `dom` reported the same id `HIDDEN`. `paint` reads what the
    /// render pass resolved; `visible` walks ancestors looking for
    /// `display:none` and `aria-hidden`, and a wrapper whose class no longer
    /// says `hidden` was still carrying it in the style tree while its subtree
    /// laid out and drew.
    ///
    /// So a check that trusts `visible` calls a control the owner can see and
    /// type into dead. Geometry plus a position is the honest question here.
    PaintsNamed,
}

/// One thing that must be true of the running panel.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    /// Stable name for this one check, so a failure can be re-run alone.
    ///
    /// The group is too coarse for that: chasing one fix meant re-running its
    /// neighbours every time, and each run drives the real app. `what` is prose
    /// and changes when the wording improves, so it cannot be the handle.
    /// This is the handle - `ps-qa qa rename-opens-editor`.
    pub id: String,
    /// Group, so a failing area can be re-run alone.
    pub group: String,
    /// What this proves, in the words you would use to report it.
    pub what: String,
    /// Press this first, to reach the surface the check is about.
    ///
    /// Checks run in sequence against one instance and start wherever the app
    /// opens, so anything not on that first surface is unreachable without a
    /// navigation step. A control that lives on only one surface fails with
    /// "no visible, enabled, sized button" when the check never got there,
    /// which reads as a missing control rather than a missing navigation.
    pub open: Option<String>,
    /// Hover this node first, if the control is revealed on hover.
    pub hover: Option<String>,
    /// Click this node, if the check is about an action.
    pub click: Option<String>,
    /// Drive `click` with a real pointer instead of a synthesised event.
    ///
    /// `AgentAction::Click` dispatches a `click` and nothing else, so a control
    /// that acts on `mousedown` reads as dead to it while working perfectly
    /// under a real pointer. The rename pencil is exactly that: it opens the
    /// editor on `mousedown` so the `role="button"` row it sits inside cannot
    /// swallow the press first. A check for it has to press, not click, or it
    /// asserts the harness rather than the app.
    pub press: bool,
    /// The node the assertion is about.
    pub subject: String,
    pub expect: Expect,
    /// Count only inside the side panel.
    ///
    /// True for anything Home also renders, which is most of the row controls.
    /// False for structure that is global by nature, such as the icon sprite.
    pub panel_only: bool,
}

/// Every check, in the order they run, read from the application's own files.
///
/// # Why these are not compiled in
///
/// They used to be Rust, in a `tests/ps-qa/` module inside this crate, which was
/// wrong twice over. It put one product's promises - its control names, its
/// section labels, its fixture names - inside a general harness, so pointing
/// this binary at a second application meant editing the harness.
/// And it made changing a selector a recompile: correcting one hardcoded
/// fixture name after a profile rebuild touched 15 references across 6 files
/// and needed a build to try.
///
/// A check is data. It is a precondition, an action and an assertion, with no
/// behaviour of its own, so it belongs in a file the application owns and
/// anybody can edit between runs.
///
/// Read from `<dir>/*.ron`, where `<dir>` is `--checks <path>`, `$PS_QA_CHECKS`,
/// or `tests/ps-qa/` under the working directory. Files are read in name order
/// and concatenated, so the group order is the filename order.
pub fn checks(dir: Option<&std::path::Path>) -> Result<Vec<Check>, String> {
    let dir = dir
        .map(std::path::Path::to_path_buf)
        .or_else(|| std::env::var_os("PS_QA_CHECKS").map(std::path::PathBuf::from))
        .unwrap_or_else(|| std::path::PathBuf::from("tests/ps-qa"));
    if !dir.is_dir() {
        return Err(format!(
            "no checks at {}. Point --checks or $PS_QA_CHECKS at the \
             application's check directory.",
            dir.display()
        ));
    }
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .map_err(|error| format!("could not read {}: {error}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "ron"))
        .collect();
    // Name order, so a run is reproducible rather than dependent on whatever
    // order the filesystem happens to hand back.
    files.sort();

    let mut all = Vec::new();
    for file in files {
        let text = std::fs::read_to_string(&file)
            .map_err(|error| format!("could not read {}: {error}", file.display()))?;
        let group: Vec<Check> = ron::from_str(&text)
            .map_err(|error| format!("could not parse {}: {error}", file.display()))?;
        all.extend(group);
    }
    Ok(all)
}

/// The side panel's left edge, in window coordinates.
///
/// The panel is a fixed 332px column on the right, and Home renders its own
/// item list with the same control names. Counting across the whole window
/// therefore mixes two lists: "Edit " matched 107 nodes and "Copy this
/// task-log entry" matched 880, most of them Home's, so a panel row appearing
/// or leaving was lost in the noise. Anything left of this is not the panel.
// `pub` in a binary crate, so the dead-code pass cannot see the two call sites
// through the module boundary and reports it unused. It is used: `matching()`
// below, and the panel resolution in `main.rs`.
#[allow(dead_code)]
pub const PANEL_LEFT: f64 = 900.0;

fn matching<'a>(nodes: &'a [SemanticNode], want: &str, panel_only: bool) -> Vec<&'a SemanticNode> {
    nodes
        .iter()
        .filter(|node| node.name.contains(want) || node.role.contains(want))
        .filter(|node| {
            !panel_only
                || node
                    .bounds
                    .is_none_or(|b| b[0] >= PANEL_LEFT || b[2] == 0.0)
        })
        .collect()
}

/// Whether a node is on screen with a box worth painting.
///
/// A zero-area box is the failure this exists to catch: present in the tree,
/// absent from the window.
fn paints(node: &SemanticNode) -> bool {
    /*
     * Geometry alone, because `visible` and the renderer disagree.
     *
     * Measured on one node at one instant: `paint` reported the rename editor
     * `300.0x21.1 at 87,236 opacity=1.00 Visible` while the semantic tree
     * reported that same id `HIDDEN`. `visible` walks ancestors for
     * `display:none` and `aria-hidden`, and a wrapper whose class no longer
     * says `hidden` was still carrying it in the style tree while its subtree
     * laid out and drew.
     *
     * Trusting the flag called controls dead that the owner can see and use -
     * the icons group reported "246 exist, none paints" for an app visibly
     * full of icons. A non-zero box at a real position is what can be checked
     * honestly from here; `ps-qa paint` is the tool for the pixels themselves.
     */
    node.bounds.is_some_and(|b| b[2] > 0.0 && b[3] > 0.0)
}

/// The verdict for one check, given the tree before and after its action.
pub fn verdict(
    check: &Check,
    before: &[SemanticNode],
    after: &[SemanticNode],
) -> Result<(), String> {
    let found = matching(after, &check.subject, check.panel_only);
    match check.expect {
        Expect::Vanishes => {
            let on_screen: Vec<&SemanticNode> =
                found.iter().copied().filter(|node| paints(node)).collect();
            if let Some(node) = on_screen.first() {
                let b = node.bounds.unwrap_or([0.0; 4]);
                return Err(format!(
                    "{:?} is still on screen at {:.0}x{:.0}; it did not close",
                    check.subject, b[2], b[3]
                ));
            }
        }
        Expect::Paints => {
            if found.is_empty() {
                return Err(format!("no node matching {:?} exists", check.subject));
            }
            if !found.iter().any(|node| paints(node)) {
                /*
                 * Say which half of "paints" failed.
                 *
                 * Hidden-but-sized and visible-but-zero-area are different
                 * bugs: the first is a node the panel deliberately keeps
                 * offscreen, the second is a control the user is meant to see
                 * and cannot. Reporting them as one message sent me looking at
                 * the wrong one.
                 */
                let hidden = found.iter().filter(|node| !node.visible).count();
                let zero = found
                    .iter()
                    .filter(|node| {
                        node.visible && !node.bounds.is_some_and(|b| b[2] > 0.0 && b[3] > 0.0)
                    })
                    .count();
                let boxes: Vec<String> = found
                    .iter()
                    .take(3)
                    .map(|node| {
                        let size = node
                            .bounds
                            .map(|b| format!("{:.0}x{:.0}", b[2], b[3]))
                            .unwrap_or_else(|| "no box".into());
                        format!("{size}{}", if node.visible { "" } else { " hidden" })
                    })
                    .collect();
                return Err(format!(
                    "{} node(s) matching {:?} exist but none paints: \
                     {hidden} hidden, {zero} visible with no area ({})",
                    found.len(),
                    check.subject,
                    boxes.join(", ")
                ));
            }
        }
        Expect::Absent => {
            if !found.is_empty() {
                return Err(format!(
                    "{} node(s) matching {:?} should not exist",
                    found.len(),
                    check.subject
                ));
            }
        }
        Expect::PaintsNamed => {
            let (role, name) = check
                .subject
                .split_once(':')
                .unwrap_or(("", &check.subject));
            let hit = after
                .iter()
                .filter(|node| node.role == role && node.name.contains(name))
                .find(|node| node.bounds.is_some_and(|b| b[2] > 0.0 && b[3] > 0.0));
            if hit.is_none() {
                let present = after
                    .iter()
                    .filter(|node| node.role == role && node.name.contains(name))
                    .count();
                return Err(format!(
                    "no {role} named {name:?} has a box ({present} in the tree)"
                ));
            }
        }
        Expect::PaintsMore => {
            let was = matching(before, &check.subject, check.panel_only)
                .into_iter()
                .filter(|node| paints(node))
                .count();
            let now = found.iter().filter(|node| paints(node)).count();
            if now <= was {
                return Err(format!(
                    "{:?} on screen went {was} -> {now}, expected one more",
                    check.subject
                ));
            }
        }
        Expect::Grows => {
            let was = matching(before, &check.subject, check.panel_only).len();
            let now = found.len();
            if now <= was {
                return Err(format!(
                    "{:?} went {was} -> {now}, expected more",
                    check.subject
                ));
            }
        }
        Expect::Holds => {
            let was = matching(before, &check.subject, check.panel_only).len();
            let now = found.len();
            if now != was {
                return Err(format!(
                    "{:?} went {was} -> {now}, expected no change",
                    check.subject
                ));
            }
        }
    }
    Ok(())
}

/// Every check, grouped, with what it drives and what it asserts.
///
/// Printed by `ps-qa list`, and generated from [`checks`] rather than written
/// down, so it cannot drift from what actually runs. This is the inventory: it
/// answers "what is covered" without launching the app, which is the question
/// that had no answer while the audit was a list of button names in a handover.
pub fn manifest(dir: Option<&std::path::Path>) -> Result<String, String> {
    let all = checks(dir)?;
    let mut out = String::new();
    let mut current = String::new();
    for check in &all {
        if check.group != current {
            current = check.group.clone();
            out.push_str(&format!("\n{current}\n"));
        }
        let action = match (&check.hover, &check.click, check.press) {
            (Some(h), Some(c), true) => format!("hover {h:?}, press {c:?}"),
            (Some(h), Some(c), false) => format!("hover {h:?}, click {c:?}"),
            (Some(h), None, _) => format!("hover {h:?}"),
            (None, Some(c), true) => format!("press {c:?}"),
            (None, Some(c), false) => format!("click {c:?}"),
            (None, None, _) => "observe only".to_owned(),
        };
        out.push_str(&format!(
            "  {:<26} {}\n{:<29}{} -> {:?} {:?}\n",
            check.id, check.what, "", action, check.expect, check.subject
        ));
    }
    out.push_str(&format!("\n{} checks in {} groups\n", all.len(), {
        let mut groups: Vec<&str> = all.iter().map(|c| c.group.as_str()).collect();
        groups.dedup();
        groups.len()
    }));
    Ok(out)
}

/// Count matching nodes per group, for the summary line.
pub fn tally<'a>(results: &[(&'a Check, Result<(), String>)]) -> HashMap<&'a str, (usize, usize)> {
    let mut by_group: HashMap<&str, (usize, usize)> = HashMap::new();
    for (check, outcome) in results {
        let entry = by_group.entry(check.group.as_str()).or_insert((0, 0));
        entry.1 += 1;
        if outcome.is_ok() {
            entry.0 += 1;
        }
    }
    by_group
}
