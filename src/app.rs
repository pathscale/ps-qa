//! What the harness has to be told about the application it is driving.
//!
//! # Why this exists
//!
//! `ps-qa` is a harness, not a test suite. Playwright does not know what a
//! "Task log" is, and neither should this: an application's surface names, its
//! collapsible sections, the region its transcript scrolls inside, are facts
//! about *that* application and belong with it.
//!
//! They were hardcoded here. `reach.rs` knew AgencyZero had surfaces called
//! Home, Settings and Analytics; `folds_a_section` listed six section names
//! from one product; the scroll commands defaulted to a region called
//! "Conversation". Pointing this binary at a second application meant editing
//! the harness, which is the definition of the wrong seam.
//!
//! # How an application describes itself
//!
//! A RON file, found by `--app <path>` or `$PS_QA_APP`, falling back to
//! `ps-qa.ron` beside the checks. Data rather than code, because the
//! application that owns these names does not want to depend on this crate to
//! state them, and because a name is not worth a recompile.
//!
//! ```ron
//! AppProfile(
//!     surfaces: [
//!         (name: "settings", opener: "Settings"),
//!         (name: "home",     opener: "Home"),
//!     ],
//!     permanent_surfaces: ["Home", "Settings", "Analytics"],
//!     sections: ["Items", "Task log"],
//!     transcript_region: "Conversation",
//!     home_opener: "Home",
//! )
//! ```
//!
//! # The default is not AgencyZero
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
    /// [`PROJECT_TAB`](crate::reach::PROJECT_TAB) stands in for "whatever the
    /// first project-like tab is", resolved at run time, for applications whose
    /// document names are not fixed strings.
    pub opener: String,
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
    pub home_opener: Option<String>,
}

impl AppProfile {
    /// Read a profile, or return an empty one if the path does not exist.
    ///
    /// A missing file is not an error: the diagnostic commands (`layout`,
    /// `dom`, `paint`) work against any Blitz application with no profile at
    /// all. Only the sweep needs to be told where to go.
    pub fn load(path: Option<&Path>) -> Result<Self, String> {
        let Some(path) = path.map(Path::to_path_buf).or_else(discover) else {
            return Ok(Self::default());
        };
        if !path.exists() {
            return Ok(Self::default());
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

/// `$PS_QA_APP`, then `ps-qa.ron` in the working directory.
fn discover() -> Option<PathBuf> {
    if let Some(from_env) = std::env::var_os("PS_QA_APP") {
        return Some(PathBuf::from(from_env));
    }
    let beside = PathBuf::from("ps-qa.ron");
    beside.exists().then_some(beside)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_profile_is_empty_rather_than_an_error() {
        let profile = AppProfile::load(Some(Path::new("/nonexistent/ps-qa.ron")))
            .expect("a missing profile is not a failure");
        assert!(profile.surfaces.is_empty());
        // And it must not pretend to know another application's sections.
        assert!(!profile.folds_a_section("Task log"));
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
            }],
            permanent_surfaces: vec!["Home".to_owned()],
            sections: vec!["Items".to_owned()],
            transcript_region: Some("Conversation".to_owned()),
            home_opener: Some("Home".to_owned()),
        };
        let text = ron::to_string(&profile).expect("serialises");
        let back: AppProfile = ron::from_str(&text).expect("parses");
        assert_eq!(back.surfaces.len(), 1);
        assert_eq!(back.sections, vec!["Items".to_owned()]);
        assert_eq!(back.transcript_region.as_deref(), Some("Conversation"));
    }
}
