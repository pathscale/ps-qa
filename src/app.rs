//! What the harness has to be told about the application it is driving.
//!
//! # Why this exists
//!
//! `ps-qa` is a harness, not a test suite. Playwright does not know what a
//! "Task log" is, and neither should this: an application's surface names, its
//! collapsible sections, the region its transcript scrolls inside, are facts
//! about *that* application and belong with it.
//!
//! They were hardcoded in the harness. `reach.rs` knew one product's three
//! surface names; `folds_a_section` listed that product's six section headers;
//! the scroll commands defaulted to its transcript region. Pointing this binary
//! at a second application meant editing the harness, which is the definition
//! of the wrong seam.
//!
//! # How an application describes itself
//!
//! A RON file, found by `--app <path>` and falling back to `ps-qa.ron` beside
//! the checks. Data rather than code, because the
//! application that owns these names does not want to depend on this crate to
//! state them, and because a name is not worth a recompile.
//!
//! ```ron
//! AppProfile(
//!     surfaces: [
//!         (name: "settings", opener: "Settings"),
//!         (name: "home",     opener: "Home"),
//!     ],
//!     permanent_surfaces: ["Home", "Settings"],
//!     sections: ["Items", "Log"],
//!     document_row_markers: [" open \u{b7} "],
//!     close_prefixes: ["Close "],
//!     row_action_prefixes: ["Rename "],
//!     fold_prefixes: ["Collapse ", "Hide "],
//!     deferred_controls: ["Start setup"],
//!     inert_controls: ["Synchronize"],
//!     transcript_region: Some("Conversation"),
//!     home_opener: Some("Home"),
//! )
//! ```
//!
//! Every field is optional. One an application does not set is simply a rule the
//! harness does not apply.
//!
//! # The default names no product
//!
//! [`AppProfile::default`] is deliberately empty apart from the structural
//! pieces every Blitz application has. An application that ships no profile
//! gets a harness that sweeps what it can find and says it found no configured
//! surfaces, rather than one quietly hunting for another product's buttons.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A surface the sweep must visit, named by the control that opens it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceSpec {
    /// Shown in the report.
    pub name: String,
    /// The accessible name of the control that navigates here.
    ///
    /// [`DYNAMIC_DOCUMENT`](crate::reach::DYNAMIC_DOCUMENT) stands in for the
    /// first user-named document, resolved at run time when fixture names are
    /// not stable.
    pub opener: String,
    /// A control unique to this surface, used to prove it is in front and to
    /// scope coverage to its semantic subtree.
    #[serde(default)]
    pub marker: Option<String>,
}

/// A control reserved for the manual release pass, and what it opens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualControl {
    /// The control's accessible name.
    pub label: String,
    /// What the application calls the thing it opens, for the worklist.
    pub command: String,
}

/// Everything about the application under test that the harness cannot infer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AppProfile {
    /// Top-level surfaces, in the order they are swept.
    pub surfaces: Vec<SurfaceSpec>,
    /// Tabs that are always present and are never a document.
    ///
    /// Used to tell a permanent surface from an opened one when reading the tab
    /// strip. Some applications double a tab's label in its accessible name
    /// (`HomeHome`), so both forms are accepted.
    pub permanent_surfaces: Vec<String>,
    /// Collapsible section headers, by their label without the count.
    ///
    /// A collapse takes everything under it off screen, so these are pressed
    /// last. Getting this list wrong is expensive: folding a section a dozen
    /// controls before the sweep reaches its rows makes every one of them read
    /// as vanished.
    pub sections: Vec<String>,
    /// The scrollable region a transcript lives in, if the application has one.
    pub transcript_region: Option<String>,
    /// The control that returns to the application's root surface.
    ///
    /// Used to recover when a sweep has walked somewhere it cannot get back
    /// from. Without it the harness simply reports the surface as unreachable.
    pub home_opener: Option<String>,
    /// Substrings that identify a document row in a list.
    ///
    /// A document's own name is not matchable: it is user data, and in a
    /// scrubbed QA profile it differs per build. What is stable is the summary
    /// the list renders beside it, so an application states the fragments of
    /// that summary instead - a count, an age, a path. An application that
    /// ships none simply has no row fallback.
    pub document_row_markers: Vec<String>,
    /// Prefixes of controls that close a surface, e.g. `Close `.
    ///
    /// These retire the pane everything else stands on, so they are swept last.
    /// A prefix rather than a whole name because the label carries the subject:
    /// `Close <document>`.
    pub close_prefixes: Vec<String>,
    /// Prefixes of controls that act on a row without opening it, e.g. `Rename `.
    ///
    /// Excluded when looking for the control that *opens* a document, because a
    /// row's rename pencil sits beside its opener and matches the same row name.
    pub row_action_prefixes: Vec<String>,
    /// Prefixes of controls that hide or fold a region, e.g. `Collapse `.
    ///
    /// Matched case-insensitively. Swept last for the same reason a section
    /// header is: pressing one takes everything beneath it off screen.
    pub fold_prefixes: Vec<String>,
    /// Controls that cannot safely run in an unattended renderer audit.
    ///
    /// A native file panel is not in the tree, no synthesised click or key
    /// reaches it, and opening one unattended leaves a run stuck behind a
    /// window it cannot dismiss. These are counted and named in the report
    /// rather than pressed, so the exceptions stay visible instead of becoming
    /// controls nobody remembers to test by hand.
    ///
    /// `label` is the control's accessible name; `command` is whatever the
    /// application wants printed beside it in the worklist.
    #[serde(alias = "native_choosers")]
    pub manual_controls: Vec<ManualControl>,
    /// Controls that must be pressed last, beyond the collapsible sections.
    ///
    /// A control that opens a new document navigates away and takes the rest of
    /// the plan with it, exactly as a collapse does. A "new document" control
    /// is the usual example; an application will have its own or none.
    pub deferred_controls: Vec<String>,
    /// Controls whose successful effect is outside the semantic tree.
    ///
    /// Matched as accessible-name prefixes. A clipboard write or backend
    /// refresh may legitimately leave the rendered component tree unchanged;
    /// the application must declare that weaker contract explicitly rather
    /// than teaching the harness product labels.
    pub inert_controls: Vec<String>,
}

impl AppProfile {
    /// Read a profile, or return an empty one if the path does not exist.
    ///
    /// A missing file is not an error: the diagnostic commands (`layout`,
    /// `dom`, `paint`) work against any Blitz application with no profile at
    /// all. Only the sweep needs to be told where to go.
    pub fn load(path: Option<&Path>) -> Result<Self, String> {
        let Some(path) = path.map(Path::to_path_buf).or_else(discover) else {
            return Err(
                "no application profile. Pass --app <path>, or put ps-qa.ron in the \
                 working directory: the harness knows nothing about any application \
                 without one, and guessing produces numbers measured against an \
                 application that does not exist."
                    .to_owned(),
            );
        };
        if !path.exists() {
            return Err(format!(
                "no application profile at {}. Pass --app <path>, or put ps-qa.ron in \
                 the working directory.",
                path.display()
            ));
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        ron::from_str(&text).map_err(|error| format!("could not parse {}: {error}", path.display()))
    }

    /// Whether a name is a section header, with or without its count.
    ///
    /// The header carries a count, so what follows the label must be a number
    /// or nothing: `Items` matches, `Items1` matches, and `Item sort between
    /// status and time` does not.
    pub fn folds_a_section(&self, name: &str) -> bool {
        self.sections.iter().any(|section| {
            name.strip_prefix(section.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.chars().all(|c| c.is_ascii_digit()))
        })
    }

    /// Whether a name is a permanent surface rather than a document.
    ///
    /// Accepts the doubled form, because a tab's accessible name may repeat its
    /// label.
    pub fn is_permanent(&self, name: &str) -> bool {
        self.permanent_surfaces.iter().any(|surface| {
            name == surface.as_str() || name == format!("{surface}{surface}").as_str()
        })
    }
}

/// `--app <path>`, then `ps-qa.ron` in the working directory.
fn discover() -> Option<PathBuf> {
    if let Some(pinned) = crate::cli::app_profile() {
        return Some(pinned);
    }
    let beside = PathBuf::from("ps-qa.ron");
    beside.exists().then_some(beside)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing profile is an error, not an empty one.
    ///
    /// This test used to assert the opposite. Loading a default meant the
    /// harness carried on describing an application nobody had described to it,
    /// and every count it reported afterwards was measured against a surface
    /// list that did not exist. Refusing to start names the missing file
    /// instead.
    #[test]
    fn an_absent_profile_is_an_error() {
        let error = AppProfile::load(Some(Path::new("/nonexistent/ps-qa.ron")))
            .expect_err("a missing profile must not load a default");
        assert!(error.contains("no application profile"), "{error}");
        assert!(error.contains("--app"), "{error}");
    }

    #[test]
    fn a_section_matches_with_or_without_its_count() {
        let profile = AppProfile {
            sections: vec!["Items".to_owned(), "Task log".to_owned()],
            ..Default::default()
        };
        assert!(profile.folds_a_section("Items"));
        assert!(profile.folds_a_section("Items12"));
        assert!(profile.folds_a_section("Task log"));
        // The trap this guards: a control whose name merely starts the same way.
        assert!(!profile.folds_a_section("Item sort between status and time"));
        assert!(!profile.folds_a_section("Itemise"));
    }

    #[test]
    fn a_permanent_surface_is_recognised_doubled() {
        let profile = AppProfile {
            permanent_surfaces: vec!["Home".to_owned(), "Settings".to_owned()],
            ..Default::default()
        };
        assert!(profile.is_permanent("Home"));
        assert!(profile.is_permanent("HomeHome"));
        assert!(!profile.is_permanent("Homely"));
        assert!(!profile.is_permanent("some project"));
    }

    #[test]
    fn a_profile_round_trips_through_ron() {
        let profile = AppProfile {
            surfaces: vec![SurfaceSpec {
                name: "home".to_owned(),
                opener: "Home".to_owned(),
                marker: Some("Dashboard heading".to_owned()),
            }],
            permanent_surfaces: vec!["Home".to_owned()],
            sections: vec!["Items".to_owned()],
            transcript_region: Some("Conversation".to_owned()),
            home_opener: Some("Home".to_owned()),
            deferred_controls: vec!["New project".to_owned()],
            inert_controls: vec!["Synchronize".to_owned()],
            document_row_markers: vec![" open · ".to_owned()],
            close_prefixes: vec!["Close ".to_owned()],
            row_action_prefixes: vec!["Rename ".to_owned()],
            fold_prefixes: vec!["Collapse ".to_owned()],
            manual_controls: vec![ManualControl {
                label: "Attach files".to_owned(),
                command: "choose_attachments".to_owned(),
            }],
        };
        let text = ron::to_string(&profile).expect("serialises");
        let back: AppProfile = ron::from_str(&text).expect("parses");
        assert_eq!(back.surfaces.len(), 1);
        assert_eq!(back.sections, vec!["Items".to_owned()]);
        assert_eq!(back.transcript_region.as_deref(), Some("Conversation"));
    }
}
