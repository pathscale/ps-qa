//! The checks themselves: what this app promises, one file per group.
//!
//! Separate from `src/`, which is the engine that runs them. A check is data -
//! a precondition, an action, and an assertion - so it belongs next to the
//! other checks rather than inside the code that judges them. Adding one means
//! editing exactly one file, and reading a group means opening exactly one.
//!
//! | file | group | covers |
//! | --- | --- | --- |
//! | [`icons`] | `icons` | artwork resolves to a box on screen |
//! | [`hover`] | `hover` | row controls that only exist while hovered |
//! | [`status`] | `status` | the status marker does not destroy its row |
//! | [`sections`] | `sections` | collapse and expand round-trip |
//! | [`tasklog`] | `tasklog` | per-row controls and paging |
//! | [`rename`] | `rename` | the in-place editor opens |
//! | [`dialog`] | `dialog` | a dialog opens *and* can be dismissed |
//! | [`delete`] | `delete` | a destructive control asks before it destroys |
//!
//! Order matters: they run top to bottom against one instance, so a later
//! check inherits whatever an earlier one left on screen.

use crate::qa::Check;

pub mod delete;
pub mod dialog;
pub mod hover;
pub mod icons;
pub mod rename;
pub mod sections;
pub mod status;
pub mod tasklog;

/// Every check, in the order they run.
pub fn all() -> Vec<Check> {
    let mut checks = Vec::new();
    checks.extend(icons::checks());
    checks.extend(hover::checks());
    checks.extend(status::checks());
    checks.extend(sections::checks());
    checks.extend(tasklog::checks());
    checks.extend(rename::checks());
    checks.extend(dialog::checks());
    checks.extend(delete::checks());
    checks
}
