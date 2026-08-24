//! Drive a repeatable interaction against a running Blitz application and
//! measure it.
//!
//! The point is to remove the human from the measurement. Hand-scrolling
//! produces numbers nobody can reproduce or compare; this sends a fixed number
//! of identical events at a fixed cadence and reports the frame window that
//! resulted.
//!
//! Two levels, as built into the bundle:
//!   `blitz.agent.control`  -> Inspect / Click / SetValue / ScrollIntoView / Key
//!   `blitz.diagnostics`    -> Observe / Snapshot / Metrics / WaitForIdle
//!
//! Requests are encoded from `blitz-control-protocol`, which is the server's
//! own definition of the wire. The previous client hand-wrote this JSON and got
//! the adjacent tagging of `AgentAction` wrong, which presented as a hung app.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use blitz_control_protocol::{
    AgentAction, AgentControlRequest, AgentSnapshot, CaptureRequest, CapturedImage, DebugResponse,
    DebugStream, DiagnosticsRequest, InputCommand, KeyPhase, Modifiers, PointerPhase,
    RendererMetrics, SemanticNode, SnapshotRequest, WheelPhase,
};
use eyre::{Result, bail, eyre};

mod app;
mod audit;
mod cli;
mod inspector;
mod qa;
mod reach;
mod report;
mod sweep;
// The checks are data, read from the application's own `tests/ps-qa/*.ron`
// at run time. See `qa::checks`.

use inspector::Client;

/// Inter-event delay. **Leave it at 0 when measuring a ceiling.** At the 1/60
/// default the harness sets the cadence and the reported frame interval
/// describes the harness rather than the application: that mistake produced
/// "49fps" on a build that actually did 308fps.
fn pace() -> Duration {
    Duration::from_secs_f64(cli::pace().max(0.0))
}

async fn sleep_pace() {
    let pace = pace();
    if !pace.is_zero() {
        tokio::time::sleep(pace).await;
    }
}

async fn metrics(client: &mut Client) -> Result<RendererMetrics> {
    match client
        .diagnostics(&DiagnosticsRequest::Metrics)
        .await?
        .response
    {
        DebugResponse::Metrics(metrics) => Ok(metrics),
        other => bail!("asked for metrics, got {other:?}"),
    }
}

/// The whole tree. `max_depth` is snake_case inside the variant even though the
/// frame wrapper is camelCase, which is the trap the shared types remove.
/// Print the live box of every named node, optionally filtered by name.
///
/// The reason this exists: a layout complaint that cannot be reproduced from
/// the markup is answered by the boxes the running app actually computed, not
/// by another screenshot. `include_layout` has been in the diagnostics snapshot
/// all along; nothing exposed it.
async fn layout(client: &mut Client, want: &str) -> Result<()> {
    let answer = client
        .diagnostics(&DiagnosticsRequest::Snapshot(SnapshotRequest {
            include_dom: true,
            include_layout: true,
            include_computed_style: false,
        }))
        .await?;
    let DebugResponse::Snapshot(snapshot) = answer.response else {
        bail!("asked for a layout snapshot, got {:?}", answer.response);
    };
    let bounds: HashMap<u64, serde_json::Value> = snapshot
        .layout
        .as_ref()
        .and_then(|value| value.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let id = row.get("nodeId")?.as_u64()?;
                    Some((id, row.get("bounds")?.clone()))
                })
                .collect()
        })
        .unwrap_or_default();

    let nodes = snapshot
        .dom
        .as_ref()
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    let mut shown = 0usize;
    for node in &nodes {
        let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let role = node.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if !want.is_empty() && !name.contains(want) && !role.contains(want) {
            continue;
        }
        let Some(id) = node.get("id").and_then(|v| v.as_u64()) else {
            continue;
        };
        let Some(box_) = bounds.get(&id) else {
            continue;
        };
        // `bounds` arrives as `[x, y, width, height]`. Reading it as an object
        // with named keys returned `None` for every one of them, and the
        // fallback was `f64::NAN`, so this printed four NaNs per row for every
        // node and never said why: a silently broken instrument, which is the
        // one thing a measurement tool must not be. Both shapes are accepted
        // now, so a protocol that grows named fields does not break it again.
        let read = |key: &str, index: usize| {
            box_.get(key)
                .or_else(|| box_.get(index))
                .and_then(|v| v.as_f64())
                .unwrap_or(f64::NAN)
        };
        let row = snapshot
            .layout
            .as_ref()
            .and_then(|value| value.as_array())
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row.get("nodeId").and_then(|value| value.as_u64()) == Some(id))
            });
        let pair = |field: &str, index: usize| {
            row.and_then(|row| row.get(field))
                .and_then(|value| value.get(index))
                .and_then(|value| value.as_f64())
                .unwrap_or(f64::NAN)
        };
        println!(
            "{:>6}  {:<16} {:>8.1} {:>8.1} {:>8.1} {:>8.1}  scroll={:.1},{:.1} range={:.1},{:.1} client={:.1},{:.1} content={:.1},{:.1}  {}",
            id,
            role,
            read("x", 0),
            read("y", 1),
            read("width", 2),
            read("height", 3),
            pair("scrollOffset", 0),
            pair("scrollOffset", 1),
            pair("scrollRange", 0),
            pair("scrollRange", 1),
            pair("clientSize", 0),
            pair("clientSize", 1),
            pair("scrollSize", 0),
            pair("scrollSize", 1),
            name.chars().take(60).collect::<String>()
        );
        shown += 1;
    }
    if shown == 0 {
        println!(
            "no named node matched {want:?} ({} in the tree)",
            nodes.len()
        );
    }
    Ok(())
}

/// What the renderer resolved every visible node to actually paint.
///
/// The reason this exists: on 2026-08-20 a window painted one flat colour and
/// took no clicks, and every other instrument said the app was healthy - 5,527
/// DOM nodes, correct content laid out in the visible band, 50fps, no GPU wait.
/// Four separate causes were proposed and eliminated, and the question "what
/// colour did the renderer think these pixels were" could not be asked at all,
/// because `include_computed_style` was in the protocol and nothing set it.
///
/// A screenshot shows the wrong colour; this says which node resolved to it,
/// which is the difference between blaming the rasteriser and finding the
/// element that asked for it. Colours arrive as `#rrggbbaa` straight from the
/// same conversion `blitz-paint` hands the rasteriser, so what is printed is
/// what was drawn, not what a stylesheet implies.
///
/// `min-area` skips the small stuff, because a full-window wash is a large box
/// and listing 5,000 glyph nodes buries it.
async fn paint(client: &mut Client, want: &str, min_area: f64) -> Result<()> {
    let answer = client
        .diagnostics(&DiagnosticsRequest::Snapshot(SnapshotRequest {
            include_dom: true,
            include_layout: true,
            include_computed_style: true,
        }))
        .await?;
    let DebugResponse::Snapshot(snapshot) = answer.response else {
        bail!("asked for a paint snapshot, got {:?}", answer.response);
    };

    let styles: HashMap<u64, serde_json::Value> = snapshot
        .computed_style
        .as_ref()
        .and_then(|value| value.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| Some((row.get("nodeId")?.as_u64()?, row.clone())))
                .collect()
        })
        .unwrap_or_default();
    if styles.is_empty() {
        bail!("the snapshot carried no computed styles; is this build's diagnostics feature on?");
    }

    let bounds: HashMap<u64, (f64, f64, f64, f64)> = snapshot
        .layout
        .as_ref()
        .and_then(|value| value.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let id = row.get("nodeId")?.as_u64()?;
                    let read = |key: &str, index: usize| {
                        row.get("bounds")
                            .and_then(|b| b.get(key).or_else(|| b.get(index)))
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0)
                    };
                    Some((
                        id,
                        (
                            read("x", 0),
                            read("y", 1),
                            read("width", 2),
                            read("height", 3),
                        ),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    let nodes = snapshot
        .dom
        .as_ref()
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();

    let mut rows: Vec<(f64, String)> = Vec::new();
    for node in &nodes {
        let Some(id) = node.get("id").and_then(|v| v.as_u64()) else {
            continue;
        };
        let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let role = node.get("role").and_then(|v| v.as_str()).unwrap_or("");
        if !want.is_empty() && !name.contains(want) && !role.contains(want) {
            continue;
        }
        let (Some(style), Some(&(x, y, w, h))) = (styles.get(&id), bounds.get(&id)) else {
            continue;
        };
        let area = w * h;
        if area < min_area {
            continue;
        }
        let field = |key: &str| {
            style
                .get(key)
                .and_then(|v| v.as_str())
                .unwrap_or("-")
                .to_string()
        };
        let opacity = style
            .get("opacity")
            .and_then(|v| v.as_f64())
            .unwrap_or(f64::NAN);
        rows.push((
            area,
            format!(
                "  {id:>11}  {role:<12} {w:>7.1}x{h:<7.1} at {x:.0},{y:.0}  bg={:<10} fg={:<10} \
                 opacity={opacity:.2} {:<12} {name}",
                field("backgroundColor"),
                field("color"),
                field("visibility"),
            ),
        ));
    }

    // Largest first: a wash covering the window is the thing being looked for,
    // and it is by definition the biggest box that resolved to that colour.
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!(
        "{} nodes, {} with computed styles, showing boxes of {min_area}px2 or more",
        nodes.len(),
        styles.len()
    );
    for (_, row) in &rows {
        println!("{row}");
    }
    if rows.is_empty() {
        println!("nothing matched");
    }
    Ok(())
}

/// The scroll state and bottom-most descendants of the visible transcript.
///
/// A screenshot can show that a reply is clipped, but cannot distinguish the
/// scroller being short of max from a child being laid out below the clip. This
/// walks the actual DOM parent chain and reports both in one read-only sample.
async fn transcript(client: &mut Client) -> Result<()> {
    let answer = client
        .diagnostics(&DiagnosticsRequest::Snapshot(SnapshotRequest {
            include_dom: true,
            include_layout: true,
            include_computed_style: false,
        }))
        .await?;
    let DebugResponse::Snapshot(snapshot) = answer.response else {
        bail!("asked for a transcript snapshot, got {:?}", answer.response);
    };
    let nodes = snapshot
        .dom
        .as_ref()
        .and_then(|value| value.as_array())
        .ok_or_else(|| eyre::eyre!("snapshot omitted DOM rows"))?;
    let rows = snapshot
        .layout
        .as_ref()
        .and_then(|value| value.as_array())
        .ok_or_else(|| eyre::eyre!("snapshot omitted layout rows"))?;
    let conversation = nodes
        .iter()
        .find(|node| {
            node.get("name").and_then(|value| value.as_str())
                == reach::profile().transcript_region.as_deref()
        })
        .and_then(|node| node.get("id").and_then(|value| value.as_u64()))
        .ok_or_else(|| eyre::eyre!("configured transcript region is absent"))?;
    let parent: HashMap<u64, Option<u64>> = nodes
        .iter()
        .filter_map(|node| {
            Some((
                node.get("id")?.as_u64()?,
                node.get("parent").and_then(|value| value.as_u64()),
            ))
        })
        .collect();
    let named: HashMap<u64, (&str, &str)> = nodes
        .iter()
        .filter_map(|node| {
            Some((
                node.get("id")?.as_u64()?,
                (
                    node.get("role")
                        .and_then(|value| value.as_str())
                        .unwrap_or(""),
                    node.get("name")
                        .and_then(|value| value.as_str())
                        .unwrap_or(""),
                ),
            ))
        })
        .collect();
    let layout: HashMap<u64, &serde_json::Value> = rows
        .iter()
        .filter_map(|row| Some((row.get("nodeId")?.as_u64()?, row)))
        .collect();
    let conversation_row = layout
        .get(&conversation)
        .ok_or_else(|| eyre::eyre!("configured transcript region has no layout row"))?;
    let pair = |row: &serde_json::Value, field: &str, index: usize| {
        row.get(field)
            .and_then(|value| value.get(index))
            .and_then(|value| value.as_f64())
            .unwrap_or(f64::NAN)
    };
    let bounds = conversation_row
        .get("bounds")
        .ok_or_else(|| eyre::eyre!("configured transcript region has no bounds"))?;
    let viewport_bottom = bounds.get(1).and_then(|v| v.as_f64()).unwrap_or(f64::NAN)
        + bounds.get(3).and_then(|v| v.as_f64()).unwrap_or(f64::NAN);
    println!(
        "transcript id={conversation} top={:.1} bottom={viewport_bottom:.1} scrollTop={:.1} max={:.1} clientHeight={:.1} scrollHeight={:.1} gapToMax={:.1}",
        bounds.get(1).and_then(|v| v.as_f64()).unwrap_or(f64::NAN),
        pair(conversation_row, "scrollOffset", 1),
        pair(conversation_row, "scrollRange", 1),
        pair(conversation_row, "clientSize", 1),
        pair(conversation_row, "scrollSize", 1),
        pair(conversation_row, "scrollRange", 1) - pair(conversation_row, "scrollOffset", 1),
    );
    let is_descendant = |mut id: u64| {
        for _ in 0..512 {
            let Some(Some(next)) = parent.get(&id) else {
                return false;
            };
            if *next == conversation {
                return true;
            }
            id = *next;
        }
        false
    };
    let mut descendants: Vec<(f64, u64, f64)> = layout
        .iter()
        .filter_map(|(id, row)| {
            if *id == conversation || !is_descendant(*id) {
                return None;
            }
            let box_ = row.get("bounds")?;
            let top = box_.get(1)?.as_f64()?;
            let height = box_.get(3)?.as_f64()?;
            Some((top + height, *id, top))
        })
        .collect();
    descendants.sort_by(|left, right| right.0.total_cmp(&left.0));
    for (bottom, id, top) in descendants.into_iter().take(12) {
        let (role, name) = named.get(&id).copied().unwrap_or(("", ""));
        println!(
            "  id={id} top={top:.1} bottom={bottom:.1} fromViewportBottom={:.1} role={role} name={}",
            bottom - viewport_bottom,
            name.chars().take(100).collect::<String>()
        );
    }
    Ok(())
}

/// Every box that sticks out of the box that contains it, worst first.
///
/// The reason this exists: "text spills past its container" is a claim about
/// one node's relationship to another node, and a three-element fixture cannot
/// express it. Two candidate mechanisms were built as fixtures and both were
/// refuted, which proved only that those two fixtures were wrong. The running
/// document already knows the answer; nothing asked it.
///
/// Horizontal by default, because vertical overflow is how a scroll container
/// works and would bury the signal. `spill v` includes the vertical axis.
async fn spill(client: &mut Client, axis: &str, tolerance: f64) -> Result<()> {
    let (snapshot, elapsed) = inspect(client).await?;
    let boxes: HashMap<u64, [f64; 4]> = snapshot
        .nodes
        .iter()
        .filter_map(|node| node.bounds.map(|bounds| (node.id, bounds)))
        .collect();

    // A spilling node is almost always an unnamed generic, so the row is
    // unreadable without saying what it sits inside. Walk up to the nearest
    // ancestor that carries a name.
    let by_id: HashMap<u64, &SemanticNode> =
        snapshot.nodes.iter().map(|node| (node.id, node)).collect();
    let describe = |mut id: u64| -> String {
        for _ in 0..12 {
            let Some(node) = by_id.get(&id) else { break };
            if !node.name.is_empty() {
                return format!(
                    "in {} \"{}\"",
                    node.role,
                    node.name.chars().take(60).collect::<String>()
                );
            }
            let Some(parent) = node.parent else { break };
            id = parent;
        }
        String::from("(no named ancestor)")
    };

    /*
     * A scroller's own offset, so scrolled-away content is not called a spill.
     *
     * Without this the tab strip reported eight children up to 1,375px left of
     * their parent, which reads as a serious layout break and is nothing at
     * all: `scroll=1186.0` against `content=2068.1` and `client=855.2` means
     * they are scrolled out of view exactly as intended. A tool that reports
     * normal scrolling as breakage costs an afternoon per reader.
     */
    let scroll_of: HashMap<u64, (f64, f64)> = {
        // A second call: the semantic snapshot carries geometry but not scroll
        // state, which only the layout snapshot has.
        let answer = client
            .diagnostics(&DiagnosticsRequest::Snapshot(SnapshotRequest {
                include_dom: false,
                include_layout: true,
                include_computed_style: false,
            }))
            .await?;
        match answer.response {
            DebugResponse::Snapshot(layout) => layout
                .layout
                .as_ref()
                .and_then(|value| value.as_array())
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            let id = row.get("nodeId")?.as_u64()?;
                            // The *range*, not the offset. A container that can
                            // scroll on an axis is one whose content is meant to
                            // exceed its box on that axis, whether or not it
                            // happens to be scrolled right now.
                            let range = row.get("scrollRange")?;
                            let x = range.get(0)?.as_f64()?;
                            let y = range.get(1)?.as_f64()?;
                            Some((id, (x, y)))
                        })
                        .collect()
                })
                .unwrap_or_default(),
            // Without offsets this reports scrolled content as spill, which is
            // wrong but not silently wrong: say so rather than pretend.
            _ => {
                eprintln!("no layout snapshot: scrolled content may read as spill");
                HashMap::new()
            }
        }
    };

    let vertical = axis.starts_with('v') || axis.starts_with('a');
    let mut by_owner: HashMap<String, (usize, f64)> = HashMap::new();
    let mut rows: Vec<(f64, String)> = Vec::new();
    for node in &snapshot.nodes {
        let (Some(child), Some(parent_id)) = (node.bounds, node.parent) else {
            continue;
        };
        let Some(parent) = boxes.get(&parent_id) else {
            continue;
        };
        // A container that scrolls on this axis is one whose content is
        // *supposed* to exceed its box on that axis, so nothing inside it can
        // have escaped. Judging it anyway reported the tab strip's eight
        // offscreen tabs as a 1,375px layout break.
        let (range_x, range_y) = scroll_of.get(&parent_id).copied().unwrap_or((0.0, 0.0));
        // A zero-sized parent is a node that has not been laid out, not a
        // container something escaped from.
        if parent[2] <= 0.0 || parent[3] <= 0.0 || child[2] <= 0.0 {
            continue;
        }
        let scrolls_x = range_x > 0.5;
        let scrolls_y = range_y > 0.5;
        let mut worst = f64::NEG_INFINITY;
        let mut how = "right";
        if !scrolls_x {
            let left = parent[0] - child[0];
            let right = (child[0] + child[2]) - (parent[0] + parent[2]);
            worst = left.max(right);
            how = if right >= left { "right" } else { "left" };
        }
        if vertical && !scrolls_y {
            let top = parent[1] - child[1];
            let bottom = (child[1] + child[3]) - (parent[1] + parent[3]);
            if top > worst {
                worst = top;
                how = "top";
            }
            if bottom > worst {
                worst = bottom;
                how = "bottom";
            }
        }
        if worst <= tolerance {
            continue;
        }
        let owner = describe(parent_id);
        let entry = by_owner.entry(owner).or_insert((0, 0.0));
        entry.0 += 1;
        entry.1 = entry.1.max(worst);
        rows.push((
            worst,
            format!(
                "{:>8.1}px {how:<6} {:<11} child[{:.0},{:.0} {:.0}x{:.0}] parent[{:.0},{:.0} {:.0}x{:.0}]  {} {}",
                worst,
                node.role,
                child[0], child[1], child[2], child[3],
                parent[0], parent[1], parent[2], parent[3],
                node.name.chars().take(40).collect::<String>(),
                describe(parent_id),
            ),
        ));
    }
    rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    println!(
        "{} nodes inspected in {elapsed:.1}ms, {} axis, tolerance {tolerance}px",
        snapshot.nodes.len(),
        if vertical { "both" } else { "horizontal" }
    );
    if rows.is_empty() {
        println!("nothing sticks out of its container");
    }
    for (_, row) in rows.iter().take(40) {
        println!("{row}");
    }
    if rows.len() > 40 {
        println!("... and {} more", rows.len() - 40);
    }
    // Parent-relative overflow is blind to the case where a box and every
    // ancestor up to the pane are all too wide together: each one fits inside
    // the next and nothing reports. So also measure everything against the
    // transcript itself, which is the edge a person can see.
    if let Some((pane, pane_box)) = snapshot
        .nodes
        .iter()
        .filter(|node| {
            reach::profile()
                .transcript_region
                .as_deref()
                .is_some_and(|r| node.name.contains(r))
        })
        .filter_map(|node| node.bounds.map(|bounds| (node, bounds)))
        .max_by(|a, b| {
            (a.1[2] * a.1[3])
                .partial_cmp(&(b.1[2] * b.1[3]))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    {
        let right = pane_box[0] + pane_box[2];
        let mut out: Vec<(f64, u64, String)> = snapshot
            .nodes
            .iter()
            .filter_map(|node| node.bounds.map(|b| (node, b)))
            // Descendants of the pane only. The project panel sits to the
            // right of the transcript and every row in it is "past" the
            // transcript's edge without overflowing anything.
            .filter(|(node, _)| {
                let mut id = node.parent;
                for _ in 0..64 {
                    match id {
                        Some(current) if current == pane.id => return true,
                        Some(current) => id = by_id.get(&current).and_then(|n| n.parent),
                        None => return false,
                    }
                }
                false
            })
            .filter_map(|(node, b)| {
                let over = (b[0] + b[2]) - right;
                (over > 0.5 && b[2] > 0.0 && b[3] > 0.0).then(|| {
                    (
                        over,
                        node.id,
                        format!("{} {}", node.role, describe(node.id)),
                    )
                })
            })
            .collect();
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // Is the transcript actually sitting on its tail? The complaint
        // "dialogs do not push the chat up" is this number: the gap between the
        // bottom of the last thing in the pane and the bottom of the pane. A
        // pinned transcript ends flush; anything else means the newest message
        // is cut off under the chrome below.
        let pane_bottom = pane_box[1] + pane_box[3];
        let deepest = snapshot
            .nodes
            .iter()
            .filter_map(|node| node.bounds.map(|b| (node, b)))
            .filter(|(node, b)| node.id != pane.id && b[2] > 0.0 && b[3] > 0.0)
            .filter(|(node, _)| {
                let mut id = node.parent;
                for _ in 0..64 {
                    match id {
                        Some(current) if current == pane.id => return true,
                        Some(current) => id = by_id.get(&current).and_then(|n| n.parent),
                        None => return false,
                    }
                }
                false
            })
            .map(|(_, b)| b[1] + b[3])
            .fold(f64::NEG_INFINITY, f64::max);
        if deepest.is_finite() {
            println!(
                "tail: last content ends at {deepest:.1}, pane ends at {pane_bottom:.1}, gap {:.1}",
                pane_bottom - deepest
            );
        }
        println!(
            "\ntranscript pane [{:.0},{:.0} {:.0}x{:.0}], right edge {right:.0}",
            pane_box[0], pane_box[1], pane_box[2], pane_box[3]
        );
        if out.is_empty() {
            println!("  nothing reaches past it");
        }
        for (over, id, what) in out.iter().take(15) {
            println!("  {over:>8.1}px past  {id:>12}  {what}");
        }
        // The chain, for the worst few. A box in the wrong place is explained by
        // whichever ancestor it was placed against, and that ancestor is never
        // the one in the row above.
        for (_, id, _) in out.iter().take(3) {
            println!("  chain for {id}:");
            let mut current = Some(*id);
            for _ in 0..16 {
                let Some(node) = current.and_then(|id| by_id.get(&id)) else {
                    break;
                };
                let b = node.bounds.unwrap_or([f64::NAN; 4]);
                println!(
                    "    {:>12} {:<12} [{:>7.1},{:>7.1} {:>7.1}x{:>6.1}]  {}",
                    node.id,
                    node.role,
                    b[0],
                    b[1],
                    b[2],
                    b[3],
                    node.name.chars().take(40).collect::<String>()
                );
                if node.id == pane.id {
                    break;
                }
                current = node.parent;
            }
        }
    }

    // The per-row list is dominated by whichever container repeats most, so
    // the grouping is what says where to look. A `truncate` row overflows by
    // design and clips in paint; a container that appears here once, deep in
    // the transcript, does not.
    if !by_owner.is_empty() {
        let mut owners: Vec<(String, (usize, f64))> = by_owner.into_iter().collect();
        owners.sort_by(|a, b| {
            b.1.1
                .partial_cmp(&a.1.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        println!("\nby container, worst first:");
        for (owner, (count, worst)) in owners {
            println!("  {count:>4} nodes  worst {worst:>7.1}px  {owner}");
        }
    }
    Ok(())
}

async fn inspect(client: &mut Client) -> Result<(AgentSnapshot, f64)> {
    let started = Instant::now();
    let answer = client
        .agent(&AgentControlRequest::Inspect {
            root: None,
            max_depth: 40,
        })
        .await?;
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    match answer.response {
        DebugResponse::AgentSnapshot(snapshot) => Ok((snapshot, elapsed)),
        other => bail!("asked for a semantic snapshot, got {other:?}"),
    }
}

/// Nodes matching `want`, each with its attributes and its ancestor chain.
///
/// `spill` says a box sticks out; it cannot say whether that is a scroller
/// doing its job or a control escaping a clip. The difference is in the
/// attributes of the ancestors — which one carries the overflow and the
/// isolation — and the semantic snapshot already reports every attribute of a
/// generic node in `value`. So this needs no new server surface: the state was
/// already on the wire and nothing printed it.
async fn dom(client: &mut Client, want: &str, depth: usize) -> Result<()> {
    if want.is_empty() {
        bail!("dom needs a substring to match");
    }
    let (snapshot, elapsed) = inspect(client).await?;
    let by_id: HashMap<u64, &SemanticNode> =
        snapshot.nodes.iter().map(|node| (node.id, node)).collect();

    let describe = |node: &SemanticNode| -> String {
        let bounds = node
            .bounds
            .map(|b| format!("[{:.0},{:.0} {:.0}x{:.0}]", b[0], b[1], b[2], b[3]))
            .unwrap_or_else(|| "[no box]".into());
        format!(
            "{} {:<10} {:<28} {bounds}{}\n      attrs: {}",
            node.id,
            node.role,
            format!("{:?}", node.name),
            if node.visible { "" } else { "  HIDDEN" },
            node.value.as_deref().unwrap_or("(none)")
        )
    };

    let matched: Vec<&SemanticNode> = snapshot
        .nodes
        .iter()
        .filter(|node| {
            node.name.contains(want)
                || node.role.contains(want)
                || node.value.as_deref().is_some_and(|v| v.contains(want))
        })
        .collect();

    println!(
        "{} of {} nodes match {want:?} (inspect {elapsed:.1}ms)\n",
        matched.len(),
        snapshot.nodes.len()
    );
    for node in &matched {
        println!("{}", describe(node));
        let mut parent = node.parent;
        for level in 0..depth {
            let Some(current) = parent.and_then(|id| by_id.get(&id)) else {
                break;
            };
            println!(
                "  {}^{} {}",
                "  ".repeat(level),
                level + 1,
                describe(current)
            );
            parent = current.parent;
        }
        println!();
    }
    Ok(())
}

async fn nodes(client: &mut Client) -> Result<usize> {
    let (snapshot, elapsed) = inspect(client).await?;
    report::show_nodes(&snapshot.nodes, elapsed);
    Ok(snapshot.nodes.len())
}

/// What each retained pane costs in nodes.
///
/// `RETAINED_PROJECT_LIMIT` keeps eight project panes mounted, and a hidden
/// pane is a full DOM subtree: it is invisible, not absent. Nobody had priced
/// one, so this walks every node to its nearest `data-retained-*` ancestor and
/// totals the subtree. The visible pane is the one a person is looking at;
/// every other line is what retention is charging for.
async fn panes(client: &mut Client) -> Result<()> {
    let (snapshot, elapsed) = inspect(client).await?;
    let by_id: HashMap<u64, &SemanticNode> =
        snapshot.nodes.iter().map(|node| (node.id, node)).collect();

    // The semantic snapshot carries no `data-*` attributes, so a pane cannot be
    // named by the attribute the shell stamps on it. It can still be found by
    // shape: a pane is a subtree hanging off a shared shell ancestor, and each
    // one contains exactly one of the region the profile names as its
    // transcript. Anchoring on that names panes without needing new server
    // surface.
    // The application names its own; a profile without one gets no pane report.
    let anchor = reach::profile()
        .transcript_region
        .clone()
        .unwrap_or_default();
    let anchor: &str = &anchor;

    let depth_of = |start: u64| -> usize {
        let mut cursor = Some(start);
        let mut depth = 0usize;
        for _ in 0..256 {
            let Some(current) = cursor.and_then(|id| by_id.get(&id)) else {
                break;
            };
            let Some(parent) = current.parent else { break };
            cursor = Some(parent);
            depth += 1;
        }
        depth
    };

    // Pane roots are the anchors' common-depth ancestors. Walk each anchor up a
    // fixed number of levels to the subtree the shell swaps, then total by root.
    let anchors: Vec<&SemanticNode> = snapshot
        .nodes
        .iter()
        .filter(|node| node.name.contains(anchor))
        .collect();

    let mut roots: HashMap<u64, (bool, Option<[f64; 4]>)> = HashMap::new();
    for anchor in &anchors {
        let mut cursor = anchor.parent;
        let mut root = anchor.id;
        // Climb to the highest ancestor that is not shared by every pane: the
        // shell container has many anchor descendants, a pane root has one.
        for _ in 0..12 {
            let Some(current) = cursor.and_then(|id| by_id.get(&id)) else {
                break;
            };
            let descendants = anchors
                .iter()
                .filter(|other| {
                    let mut walk = Some(other.id);
                    for _ in 0..256 {
                        let Some(step) = walk.and_then(|id| by_id.get(&id)) else {
                            return false;
                        };
                        if step.id == current.id {
                            return true;
                        }
                        walk = step.parent;
                    }
                    false
                })
                .count();
            if descendants > 1 {
                break;
            }
            root = current.id;
            cursor = current.parent;
        }
        let node = by_id.get(&root).copied();
        roots.insert(
            root,
            (
                node.map(|n| n.visible).unwrap_or(false),
                node.and_then(|n| n.bounds),
            ),
        );
    }

    let mut totals: HashMap<u64, usize> = HashMap::new();
    let mut unattributed = 0usize;
    for node in &snapshot.nodes {
        let mut cursor = Some(node.id);
        let mut found = None;
        for _ in 0..256 {
            let Some(current) = cursor.and_then(|id| by_id.get(&id)) else {
                break;
            };
            if roots.contains_key(&current.id) {
                found = Some(current.id);
                break;
            }
            cursor = current.parent;
        }
        match found {
            Some(root) => *totals.entry(root).or_default() += 1,
            None => unattributed += 1,
        }
    }

    let mut rows: Vec<(u64, usize)> = totals.into_iter().collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.1));
    println!(
        "{} nodes total, {} panes found via {anchor:?} (inspect {elapsed:.1}ms)\n",
        snapshot.nodes.len(),
        rows.len()
    );
    let mut hidden_cost = 0usize;
    for (root, count) in &rows {
        let (visible, bounds) = roots.get(root).copied().unwrap_or((false, None));
        let box_text = bounds
            .map(|b| format!("[{:.0},{:.0} {:.0}x{:.0}]", b[0], b[1], b[2], b[3]))
            .unwrap_or_else(|| "[no box]".into());
        println!(
            "  {count:>6}  node {root:<14} {:<8} depth {:<3} {box_text}",
            if visible { "VISIBLE" } else { "hidden" },
            depth_of(*root)
        );
        if !visible {
            hidden_cost += count;
        }
    }
    println!("\n  {unattributed:>6}  outside any pane (chrome, tab strip, overlays)");
    println!(
        "  {hidden_cost:>6}  in hidden panes = {:.0}% of the tree",
        100.0 * hidden_cost as f64 / snapshot.nodes.len().max(1) as f64
    );
    Ok(())
}

/// A fixed scroll burst, paced like a trackpad rather than a firehose.
/// Put the pointer over a named node, so the next wheel event goes to the
/// scroller under it.
///
/// A wheel event carries no coordinates. It lands on whatever the document
/// last saw the pointer over, which after a fresh launch is nothing, and the
/// scroll then goes to the root or to whichever container happened to be
/// hovered. Three scroll sweeps over a transcript changed the mounted rows not
/// at all and read as "the bug does not reproduce", when the transcript had
/// never been scrolled.
async fn hover_over(client: &mut Client, want: &str) -> Result<bool> {
    // "x,y" targets a point directly. A scroll container often has no
    // accessible name, so naming is not always enough to put the pointer inside
    // the thing you mean to scroll: a repeated label can find a button in the
    // sidebar and every wheel event went there.
    if let Some((x, y)) = want.split_once(',')
        && let (Ok(x), Ok(y)) = (x.trim().parse::<f64>(), y.trim().parse::<f64>())
    {
        {
            client
                .agent(&AgentControlRequest::Act(AgentAction::Input(
                    InputCommand::Pointer {
                        phase: PointerPhase::Move,
                        x,
                        y,
                        button: 0,
                        modifiers: Modifiers::default(),
                    },
                )))
                .await?;
            println!("pointer at {x:.0},{y:.0}");
            return Ok(true);
        }
    }
    let (snapshot, _) = inspect(client).await?;
    let Some(node) = snapshot
        .nodes
        .iter()
        .filter(|node| node.visible && node.name.contains(want))
        .filter_map(|node| node.bounds.map(|bounds| (node, bounds)))
        .max_by(|a, b| {
            (a.1[2] * a.1[3])
                .partial_cmp(&(b.1[2] * b.1[3]))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(node, bounds)| (node.name.clone(), bounds))
    else {
        println!("no visible node named {want:?} to hover; wheel goes wherever it goes");
        return Ok(false);
    };
    let (name, bounds) = node;
    let (x, y) = (bounds[0] + bounds[2] / 2.0, bounds[1] + bounds[3] / 2.0);
    client
        .agent(&AgentControlRequest::Act(AgentAction::Input(
            InputCommand::Pointer {
                phase: PointerPhase::Move,
                x,
                y,
                button: 0,
                modifiers: Modifiers::default(),
            },
        )))
        .await?;
    println!("pointer over {name:?} at {x:.0},{y:.0}");
    Ok(true)
}

async fn scroll(client: &mut Client, ticks: usize, delta: f64) -> Result<()> {
    // Say what the pace is, every time, before any number is printed.
    //
    // The default sends a wheel event every 1/60s, so the app is asked for
    // roughly 60 frames a second and answers about 53 once the sleep overhead
    // is counted. Read without this line, that 53 looks like the application
    // failing to keep up on a 120Hz display, and the `missed_refreshes` figure
    // beside it appears to confirm it. Both are describing this loop.
    //
    // `BENCH_PACE=0` sends as fast as the app accepts, which is what measures
    // the application: it reaches 120fps with no missed refreshes.
    let pace = pace();
    if pace.is_zero() {
        println!("pace: unpaced (BENCH_PACE=0) - measures app throughput");
    } else {
        println!(
            "pace: {:.2}ms between events ({:.0} Hz requested); fps and missed_refreshes \
             below describe this pace, not the app's limit. BENCH_PACE=0 to remove it",
            pace.as_secs_f64() * 1000.0,
            1.0 / pace.as_secs_f64(),
        );
    }

    let mut latencies = Vec::with_capacity(ticks);
    for _ in 0..ticks {
        let started = Instant::now();
        client
            .agent(&AgentControlRequest::Act(AgentAction::Input(
                InputCommand::Wheel {
                    delta_x: 0.0,
                    delta_y: delta,
                    phase: WheelPhase::Moved,
                    modifiers: Modifiers::default(),
                },
            )))
            .await?;
        latencies.push(started.elapsed().as_secs_f64() * 1000.0);
        sleep_pace().await;
    }
    report::show_latencies("wheel events", ticks, &mut latencies);
    Ok(())
}

/// The painted textbox whose semantic name mentions `want` on the active layer.
fn find_text_field<'a>(nodes: &'a [SemanticNode], want: &str) -> Option<&'a SemanticNode> {
    let modal_scope: HashSet<u64> = reach::dismissers(nodes)
        .first()
        .map(|(id, _)| reach::enclosing_dialog(nodes, *id))
        .unwrap_or_default()
        .into_iter()
        .collect();
    let surface_scope: HashSet<u64> = reach::surfaces()
        .iter()
        .find(|surface| reach::on_surface(nodes, surface))
        .map(|surface| reach::on_surface_subtree(nodes, surface))
        .unwrap_or_default()
        .into_iter()
        .collect();
    let fields: Vec<&SemanticNode> = nodes
        .iter()
        .filter(|node| {
            matches!(node.role.as_str(), "textbox" | "textarea" | "input")
                && node.enabled
                && node
                    .bounds
                    .is_some_and(|bounds| bounds[2] > 0.0 && bounds[3] > 0.0)
        })
        .collect();

    let matches_name = |node: &&SemanticNode| want.is_empty() || selector_matches_node(node, want);
    for scope in [&modal_scope, &surface_scope] {
        if let Some(field) = fields
            .iter()
            .find(|node| matches_name(node) && scope.contains(&node.id))
        {
            return Some(field);
        }
    }
    fields.into_iter().find(matches_name)
}

/// Drive real key events into a focused text field and price them.
///
/// Typing is the interaction the composer autosizes on: it writes
/// `style.height`, reads `scrollHeight`, and writes it again, so every
/// keystroke forces a synchronous layout resolve. Scrolling never exercises
/// that path, which is why a scroll benchmark cannot stand in for this one.
/// Send a named key, optionally repeated, after focusing something inside a
/// named container.
///
/// Wheel events could not scroll the transcript: they carry no coordinates, and
/// pointing them at the pane still moved nothing. Page Up does, because it goes
/// to the focused scroller, and without it every attempt to reach the rows the
/// owner was looking at meant asking a person to scroll. A bug that only
/// reproduces by hand is a bug that gets one measurement per message.
async fn press_key(client: &mut Client, name: &str, count: usize, over: &str) -> Result<()> {
    // A key goes to the focused node, so click the container first. Clicking a
    // scroll container's own body focuses it without activating anything: the
    // transcript section carries `tabindex="0"` for exactly this.
    let (snapshot, _) = inspect(client).await?;
    // A control whose only label is `sr-only` reaches the semantic tree with an
    // empty name, so no substring can address it. The slider is one, which is
    // why targeting by node id has to be possible at all.
    let by_id = over.parse::<u64>().ok().filter(|id| {
        snapshot.nodes.iter().any(|node| {
            node.id == *id
                && node
                    .bounds
                    .is_some_and(|bounds| bounds[2] > 0.0 && bounds[3] > 0.0)
        })
    });
    if let Some(target) = by_id.or_else(|| {
        snapshot
            .nodes
            .iter()
            .filter(|node| {
                !over.is_empty()
                    && selector_matches_node(node, over)
                    && node
                        .bounds
                        .is_some_and(|bounds| bounds[2] > 0.0 && bounds[3] > 0.0)
            })
            .filter_map(|node| node.bounds.map(|b| (node, b)))
            .max_by(|a, b| {
                (a.1[2] * a.1[3])
                    .partial_cmp(&(b.1[2] * b.1[3]))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(node, _)| node.id)
    }) {
        client
            .agent(&AgentControlRequest::Act(AgentAction::Click {
                node_id: target,
            }))
            .await?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        println!("focused node {target} for {name} x{count}");
    } else {
        println!("no visible node named {over:?}; sending {name} to whatever has focus");
    }

    // `key` and `code` are both what the DOM calls them. They are not
    // interchangeable and sending the wrong one is a silent no-op.
    let (key, code) = match name.to_ascii_lowercase().as_str() {
        "pageup" | "pgup" => ("PageUp", "PageUp"),
        "pagedown" | "pgdn" => ("PageDown", "PageDown"),
        "home" => ("Home", "Home"),
        "end" => ("End", "End"),
        "up" | "arrowup" => ("ArrowUp", "ArrowUp"),
        "down" | "arrowdown" => ("ArrowDown", "ArrowDown"),
        "left" | "arrowleft" => ("ArrowLeft", "ArrowLeft"),
        "right" | "arrowright" => ("ArrowRight", "ArrowRight"),
        "tab" => ("Tab", "Tab"),
        "enter" => ("Enter", "Enter"),
        "escape" | "esc" => ("Escape", "Escape"),
        other => {
            bail!(
                "unknown key {other:?}: pageup, pagedown, home, end, up, down, left, right, tab, enter, escape"
            )
        }
    };

    for _ in 0..count {
        for phase in [KeyPhase::Down, KeyPhase::Up] {
            client
                .agent(&AgentControlRequest::Act(AgentAction::Input(
                    InputCommand::Key {
                        phase,
                        key: key.to_string(),
                        code: code.to_string(),
                        modifiers: Modifiers::default(),
                    },
                )))
                .await?;
        }
        sleep_pace().await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    Ok(())
}

async fn type_keys(client: &mut Client, count: usize, want: &str) -> Result<()> {
    let (snapshot, _) = inspect(client).await?;
    let Some(field) = find_text_field(&snapshot.nodes, want) else {
        bail!("no enabled, visible text field found; open a tab with a composer");
    };
    println!(
        "typing into node {} role={} name={}",
        field.id,
        field.role,
        report::py_repr(&field.name.chars().take(40).collect::<String>())
    );
    client
        .agent(&AgentControlRequest::Act(AgentAction::Click {
            node_id: field.id,
        }))
        .await?;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let before = metrics(client).await?;
    let mut latencies = Vec::with_capacity(count);
    for index in 0..count {
        let letter = (b'a' + (index % 26) as u8) as char;
        let started = Instant::now();
        for phase in [KeyPhase::Down, KeyPhase::Up] {
            client
                .agent(&AgentControlRequest::Act(AgentAction::Input(
                    InputCommand::Key {
                        phase,
                        key: letter.to_string(),
                        code: format!("Key{}", letter.to_ascii_uppercase()),
                        modifiers: Modifiers::default(),
                    },
                )))
                .await?;
        }
        latencies.push(started.elapsed().as_secs_f64() * 1000.0);
        sleep_pace().await;
    }
    let after = metrics(client).await?;

    report::show_latencies("keystrokes", count, &mut latencies);
    report::show("before", &before);
    report::show("after", &after);
    report::show_delta(&before, &after, count);
    Ok(())
}

/// Set literal text on an exact semantic text-field node.
async fn type_text(client: &mut Client, want: &str, text: &str) -> Result<u64> {
    let (snapshot, _) = inspect(client).await?;
    let field = find_text_field(&snapshot.nodes, want)
        .ok_or_else(|| eyre!("no enabled, visible text field matching {want:?}"))?;
    if cli::trace() {
        println!("        setting {want:?} (id {})", field.id);
    }
    let answer = client
        .agent(&AgentControlRequest::Act(AgentAction::SetValue {
            node_id: field.id,
            value: text.to_owned(),
        }))
        .await?;
    if let DebugResponse::Error(error) = answer.response {
        bail!("{} ({})", error.message, error.code);
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    Ok(field.id)
}

/// Price a single click, such as switching to a tab.
///
/// A tab switch flips `display: none` to `flex` over that tab's whole subtree,
/// so taffy lays out in one pass everything the tab retained while hidden. That
/// is a different cost from typing and needs its own measurement.
async fn click_named(client: &mut Client, want: &str) -> Result<()> {
    let (snapshot, _) = inspect(client).await?;
    let wanted = want.to_lowercase();
    let Some(target) = snapshot
        .nodes
        .iter()
        .find(|node| node.name.to_lowercase().contains(&wanted) && node.visible && node.enabled)
    else {
        bail!(
            "no visible, enabled node whose name contains {}",
            report::py_repr(want)
        );
    };
    println!(
        "clicking node {} role={} name={}",
        target.id,
        target.role,
        report::py_repr(&target.name.chars().take(50).collect::<String>())
    );

    // A click is dispatched at the node's coordinates, so a node scrolled out
    // of the viewport gets a `pointerdown` at a point nothing is at and no
    // click at all. "Show 12 earlier messages" sat at y=-2246 and every attempt
    // to press it read as the button doing nothing.
    let offscreen = target
        .bounds
        .is_some_and(|b| b[1] + b[3] < 0.0 || b[0] + b[2] < 0.0);
    let target_id = target.id;
    if offscreen {
        println!("  offscreen, scrolling it into view first");
        client
            .agent(&AgentControlRequest::Act(AgentAction::ScrollIntoView {
                node_id: target_id,
            }))
            .await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    let before = metrics(client).await?;
    let started = Instant::now();
    client
        .agent(&AgentControlRequest::Act(AgentAction::Click {
            node_id: target_id,
        }))
        .await?;
    let ack = started.elapsed().as_secs_f64() * 1000.0;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let after = metrics(client).await?;

    println!("click acked in {ack:.1}ms");
    report::show("before", &before);
    report::show("after", &after);
    report::show_delta(&before, &after, 1);
    Ok(())
}

/// Drive every panel check and report what the renderer did.
///
/// Each check runs against the live tree, and the three steps are separated on
/// purpose: hovering is what makes the row controls exist at all, and a check
/// that skips it reports a missing feature rather than a test driving the app
/// wrongly. That mistake is why the hover regression shipped.
///
/// Returns the number of failures, so the caller can set an exit code.
async fn run_qa(
    client: &mut Client,
    group: Option<&str>,
    checks_dir: Option<&std::path::Path>,
) -> Result<usize> {
    let all = qa::checks(checks_dir).map_err(|error| eyre!(error))?;
    // A group *or* one check's id, so chasing a single failure does not mean
    // re-running its neighbours against the real app every time.
    let selected: Vec<&qa::Check> = all
        .iter()
        .filter(|check| group.is_none_or(|want| check.group == want || check.id == want))
        .collect();
    if selected.is_empty() {
        let mut names: Vec<String> = all
            .iter()
            .map(|check| format!("{} ({})", check.id, check.group))
            .collect();
        names.sort();
        bail!(
            "no check or group matching {group:?}. known:\n  {}",
            names.join("\n  ")
        );
    }

    let mut results: Vec<(&qa::Check, std::result::Result<(), String>)> = Vec::new();

    for check in selected {
        /*
         * Navigate first, and snapshot *after*, or the baseline is the wrong
         * surface entirely and every count is measured against a screen the
         * check is not about.
         *
         * A failure here is the check's own setup failing, reported as such:
         * "could not open" rather than a verdict about the control, which is a
         * distinction that cost a round of chasing a control that was simply
         * on another surface.
         */
        /*
         * Navigation is best-effort: already being on the surface is success.
         *
         * Checks run in sequence, so a later one often inherits exactly the
         * screen it would have navigated to, and the control it navigates *by*
         * is no longer on it. Treating that as a failure reported "could not
         * open" for a check whose own press had already worked - the verdict
         * was right there and got overwritten by its own setup.
         *
         * A genuinely unreachable surface still fails, one step later, when
         * the check cannot find the control it is about.
         */
        let mut open_error = None;
        if let Some(want) = check.open.as_deref() {
            /*
             * "Already there" is judged by the *click target*, not the subject.
             *
             * Judged by the subject, a check can decide it arrived because a
             * different surface renders the same word, skip the
             * navigation, and then could not find the control it was about.
             * The control the check is going to drive is the honest test of
             * whether the surface is in front.
             */
            let want_here: &str = check.click.as_deref().unwrap_or(&check.subject);
            let destination = surface_for_opener(want);
            let (here, _) = inspect(client).await?;
            let arrived = destination.map_or_else(
                || painted_named(&here.nodes, want_here),
                |surface| reach::on_surface(&here.nodes, surface),
            );
            if !arrived {
                /*
                 * Two steps, because the opener may not be on this surface.
                 *
                 * A document can be opened from a root-surface row, and once
                 * the document is in front that row is gone, so a check that navigates by it
                 * failed with "no visible, enabled, sized button" for a
                 * surface that was one press away. The configured root opener
                 * provides the recovery path.
                 */
                /*
                 * A tab is pressed, a row is double clicked, and which one
                 * this is cannot be known from the name - so try the cheap
                 * gesture first and escalate.
                 *
                 * Double clicking a tab-strip entry may toggle it rather than
                 * navigating, while a single row click may fold it, so
                 * committing to either gesture broke the other set of checks.
                 */
                let _ = click_named_quiet(client, want).await;
                settle(client, None).await?;
                let (now, _) = inspect(client).await?;
                let there = destination.map_or_else(
                    || painted_named(&now.nodes, want_here),
                    |surface| reach::on_surface(&now.nodes, surface),
                );
                if !there && open_named(client, want).await.is_err() {
                    if let Some(home) = reach::profile().home_opener.as_deref() {
                        let _ = click_named_quiet(client, home).await;
                    }
                    settle(client, None).await?;
                    if let Err(error) = open_named(client, want).await {
                        open_error = Some(format!("could not open {want:?}: {error}"));
                    }
                }
            }
            settle(client, None).await?;
        }

        /*
         * A check must be independent of whichever disclosure a previous
         * check left closed. The application profile already identifies its
         * collapsible sections for inventory and coverage; use that same
         * app-owned information when the check's action target is not exposed.
         *
         * Only expand when the target is missing. This preserves intentional
         * collapse/expand round trips: an `Expand …` action remains available
         * after the preceding check closed its section and is not pre-empted.
         */
        let action_target = check
            .click
            .as_deref()
            .or(check.type_into.as_deref())
            .or(check.key_on.as_deref())
            .unwrap_or(&check.subject);
        let (current, _) = inspect(client).await?;
        if !painted_named(&current.nodes, action_target)
            && let Some(surface) = reach::surfaces()
                .iter()
                .find(|surface| reach::on_surface(&current.nodes, surface))
        {
            let opened = expand_everything(client, surface).await?;
            if cli::trace() && opened > 0 {
                println!("        opened {opened} collapsed section(s) for {action_target:?}");
            }
            settle(client, None).await?;
        }

        let (expanded, _) = inspect(client).await?;
        if !painted_named(&expanded.nodes, action_target)
            && let Some(surface) = reach::surfaces()
                .iter()
                .find(|surface| reach::on_surface(&expanded.nodes, surface))
        {
            let reveals = reveal_deferred_content(client, surface, action_target).await?;
            if cli::trace() && reveals > 0 {
                println!(
                    "        revealed deferred content for {action_target:?} in {reveals} step(s)"
                );
            }
            settle(client, None).await?;
        }

        let (before, _) = inspect(client).await?;

        // Hover first: the row actions do not exist until `pointerenter`.
        //
        // Aimed at a node inside the panel column, not merely one whose name
        // matches. Another retained surface may render the same control names,
        // so hovering by name alone can land in the wrong list.
        if let Some(want) = check.hover.as_deref() {
            let (tree, _) = inspect(client).await?;
            let target = tree
                .nodes
                .iter()
                .find(|node| {
                    node.name.contains(want)
                        && node.visible
                        && node.bounds.is_some_and(|b| b[2] > 0.0 && b[3] > 0.0)
                })
                .map(|node| node.id);
            let Some(node_id) = target else {
                bail!("no visible, sized node matching {want:?} to hover");
            };
            client
                .agent(&AgentControlRequest::Act(AgentAction::Hover { node_id }))
                .await?;
            tokio::time::sleep(Duration::from_millis(150)).await;
        }

        // Then the action, if this check is about one. A click that cannot be
        // dispatched is itself a failure, not a skip.
        let mut click_error = None;
        let mut action_target = None;
        let mut action_node_id = None;
        if let Some(want) = check.click.as_deref() {
            if check.expect == qa::Expect::TargetPaints && !check.press {
                let (tree, _) = inspect(client).await?;
                let wanted = want.to_lowercase();
                action_target = tree
                    .nodes
                    .iter()
                    .find(|node| {
                        node.name.to_lowercase().contains(&wanted) && node.visible && node.enabled
                    })
                    .map(|node| node.name.clone());
            }
            let driven = if check.press {
                press_named(client, want).await.map(|()| None)
            } else {
                click_named_quiet(client, want).await.map(Some)
            };
            match driven {
                Ok(node_id) => action_node_id = node_id,
                Err(error) => {
                    let how = if check.press { "press" } else { "click" };
                    click_error = Some(format!("could not {how} {want:?}: {error}"));
                }
            }
            /*
             * Long enough for the *slowest* thing a control opens.
             *
             * 600ms was tuned on dialogs, which appear immediately. The rename
             * editor does not: traced against a running build, the press
             * landed and the editor was on screen at 300x21 a moment later,
             * but the `after` snapshot had already been taken and the check
             * reported the control dead. A settle time that varies by what is
             * being driven is how a working control reads as broken, so this
             * is the slow case for everything.
             */
            settle(client, None).await?;
        }

        if click_error.is_none()
            && let Some(text) = check.text.as_deref()
        {
            if let Some(field) = check.type_into.as_deref() {
                match type_text(client, field, text).await {
                    Ok(node_id) => action_node_id = Some(node_id),
                    Err(error) => {
                        click_error = Some(format!("could not type into {field:?}: {error}"));
                    }
                }
            } else {
                click_error = Some("text requires type_into".to_owned());
            }
        }
        if click_error.is_none()
            && let Some(key) = check.key.as_deref()
        {
            let target = check
                .key_on
                .as_deref()
                .or(check.type_into.as_deref())
                .unwrap_or("");
            if let Err(error) = press_key(client, key, 1, target).await {
                click_error = Some(format!("could not send {key:?}: {error}"));
            }
        }
        if check.text.is_some() || check.key.is_some() {
            settle(client, None).await?;
        }

        let action_error = open_error.or(click_error);
        let after = if action_error.is_none() {
            settle_for_outcome(
                client,
                check,
                &before.nodes,
                action_target.as_deref(),
                action_node_id,
            )
            .await?
        } else {
            inspect(client).await?.0
        };
        let outcome = match action_error {
            Some(error) => Err(error),
            None => outcome_verdict(
                check,
                &before.nodes,
                &after.nodes,
                action_target.as_deref(),
                action_node_id,
            ),
        };
        results.push((check, outcome));
    }

    let failed = results.iter().filter(|(_, out)| out.is_err()).count();
    let tally = qa::tally(&results);
    let mut groups: Vec<_> = tally.iter().collect();
    groups.sort_by_key(|(name, _)| *name);

    /*
     * TOON, for a caller that is a program - usually a language model.
     *
     * The column format below is for a person: reading it back means splitting
     * on whitespace, which loses any field containing a space. `what` always
     * contains one and a failure message nearly always does. That mis-parse is
     * not hypothetical - it is how a node at `[0,58 0x0]` got read as painting.
     *
     * TOON rather than JSON because a uniform array declares its length and
     * field names once, then spends one line per row instead of repeating every
     * key in every object. For a run of two dozen checks that is a large
     * fraction of the tokens, and the declared `[n]` lets the reader verify it
     * received every row rather than a truncated list.
     *
     * Nesting is why this is not flat TSV: reporting wants a check to carry its
     * own detail, and TOON keeps that expressible without giving up the tabular
     * economy.
     *
     * Encoded by `toon-format` rather than by hand. Quoting is the whole
     * problem here - a check description and a failure message both routinely
     * contain the delimiter ("Edit " went 19 -> 21, expected no change) - and a
     * hand-written emitter is a guess at the specification that drifts from it
     * silently. The crate is generic over `serde::Serialize`, so anything with
     * a `Serialize` impl encodes without a bespoke path.
     */

    let report = Report {
        passed: results.len() - failed,
        failed,
        groups: groups
            .into_iter()
            .map(|(name, (passed, total))| GroupRow {
                name: name.to_string(),
                passed: *passed,
                total: *total,
            })
            .collect(),
        checks: results.iter().map(CheckRow::from).collect(),
    };
    println!(
        "{}",
        toon_format::encode_default(&report).map_err(|e| eyre!(e.to_string()))?
    );
    Ok(failed)
}

fn outcome_verdict(
    check: &qa::Check,
    before: &[SemanticNode],
    after: &[SemanticNode],
    action_target: Option<&str>,
    action_node_id: Option<u64>,
) -> std::result::Result<(), String> {
    if check.expect == qa::Expect::TargetPaints {
        let Some(subject) = action_target else {
            return Err("could not resolve the exact click target".to_owned());
        };
        let mut targeted = check.clone();
        targeted.subject = subject.to_owned();
        targeted.expect = qa::Expect::Paints;
        qa::verdict(&targeted, before, after)
    } else if check.expect == qa::Expect::ValueChanges
        && let Some(node_id) = action_node_id
    {
        qa::value_changed(node_id, before, after)
    } else {
        qa::verdict(check, before, after)
    }
}

/// Wait for the declared result, not merely for a tree that already contains
/// the subject.
///
/// A refresh indicator exists both before and after its button is activated.
/// Waiting for that node to paint therefore returned immediately, even when
/// its backend read was still running. Provider discovery can legitimately
/// take seven seconds on a clean macOS runner, so keep sampling the exact
/// verdict for ten seconds and return the last snapshot for a precise failure.
async fn settle_for_outcome(
    client: &mut Client,
    check: &qa::Check,
    before: &[SemanticNode],
    action_target: Option<&str>,
    action_node_id: Option<u64>,
) -> Result<AgentSnapshot> {
    const OUTCOME_TIMEOUT: Duration = Duration::from_secs(10);
    let deadline = tokio::time::Instant::now() + OUTCOME_TIMEOUT;
    loop {
        let (after, _) = inspect(client).await?;
        if outcome_verdict(check, before, &after.nodes, action_target, action_node_id).is_ok()
            || tokio::time::Instant::now() >= deadline
        {
            return Ok(after);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Resolve an application-owned opener to the surface it promises to show.
///
/// Document names are fixture data, so they match the profile's dynamic
/// document surface after literal permanent-surface openers are considered.
fn surface_for_opener(want: &str) -> Option<&'static reach::Surface> {
    reach::surfaces()
        .iter()
        .find(|surface| surface.opener == want)
        .or_else(|| {
            reach::surfaces()
                .iter()
                .find(|surface| surface.opener == reach::DYNAMIC_DOCUMENT)
        })
}

/// Whether a named check target currently occupies a box in the live tree.
///
/// `role:name` is accepted for precise subjects such as rename textboxes; bare
/// names retain the normal substring behavior used by application manifests.
fn painted_named(nodes: &[SemanticNode], want: &str) -> bool {
    let (role, name) = want.split_once(':').unwrap_or(("", want));
    nodes.iter().any(|node| {
        (role.is_empty() || node.role == role)
            && node.name.contains(name)
            && node
                .bounds
                .is_some_and(|bounds| bounds[2] > 0.0 && bounds[3] > 0.0)
    })
}

/// Click one node by id, with no name lookup in between.
async fn click_by_id(client: &mut Client, node_id: u64) -> Result<()> {
    let answer = client
        .agent(&AgentControlRequest::Act(AgentAction::Click { node_id }))
        .await?;
    if let DebugResponse::Error(error) = answer.response {
        bail!("{} ({})", error.message, error.code);
    }
    Ok(())
}

/// Wait until the tree stops changing, rather than guessing how long to sleep.
///
/// A fixed settle is wrong in both directions: too short and a working control
/// reads as dead because its result had not painted when the snapshot was
/// taken, too long and every check pays for the slowest one. The rename editor
/// was still failing at 1200ms while the state *after* the run showed it open,
/// which is the failure this removes.
///
/// Two consecutive identical reads, because one is not enough: an action that
/// clears before it fills reports a stable tree in the gap between.
async fn settle(client: &mut Client, want: Option<&str>) -> Result<()> {
    /*
     * Wait for the *subject* where there is one, not for the tree.
     *
     * A whole-tree count is stable while a single node is mid-open, so it
     * returned early and the check read a screen the action had not reached
     * yet: measured immediately after a failing run, the rename editor was
     * painting at 650x23 while the check had just reported it absent.
     */
    if let Some(subject) = want {
        let (role, name) = subject.split_once(':').unwrap_or(("", subject));
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let (snapshot, _) = inspect(client).await?;
            let painted = snapshot.nodes.iter().any(|n| {
                (role.is_empty() || n.role == role)
                    && n.name.contains(name)
                    && n.bounds.is_some_and(|b| b[2] > 0.0 && b[3] > 0.0)
            });
            if painted {
                return Ok(());
            }
        }
        return Ok(());
    }
    let mut last = 0usize;
    let mut stable = 0;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let (snapshot, _) = inspect(client).await?;
        let now = snapshot
            .nodes
            .iter()
            .filter(|n| n.visible && n.bounds.is_some_and(|b| b[2] > 0.0 && b[3] > 0.0))
            .count();
        if now == last {
            stable += 1;
            if stable == 2 {
                return Ok(());
            }
        } else {
            stable = 0;
            last = now;
        }
    }
    Ok(())
}

/// A finished run, as a machine reads it.
///
/// Serialized rather than printed field by field: `toon-format` is generic over
/// `serde::Serialize`, so the shape below *is* the output format, and there is
/// no second hand-written description of it to drift.
#[derive(serde::Serialize)]
struct Report {
    passed: usize,
    failed: usize,
    groups: Vec<GroupRow>,
    checks: Vec<CheckRow>,
}

#[derive(serde::Serialize)]
struct GroupRow {
    name: String,
    passed: usize,
    total: usize,
}

/// One check's outcome. Uniform, so TOON emits it as a table: the field names
/// are declared once and each check costs a single line.
#[derive(serde::Serialize)]
struct CheckRow {
    verdict: &'static str,
    group: String,
    id: String,
    error: String,
    what: String,
}

impl From<&(&qa::Check, std::result::Result<(), String>)> for CheckRow {
    fn from((check, outcome): &(&qa::Check, std::result::Result<(), String>)) -> Self {
        Self {
            verdict: if outcome.is_ok() { "pass" } else { "fail" },
            group: check.group.clone(),
            id: check.id.clone(),
            error: outcome.clone().err().unwrap_or_default(),
            what: check.what.clone(),
        }
    }
}

/// One control, as `find` reports it.
#[derive(serde::Serialize)]
struct FoundRow {
    id: u64,
    role: String,
    name: String,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    /// `visible`, `hidden`, `disabled`, `0x0`, `offscreen`, comma-joined. The
    /// question behind most lookups is "why can nobody press this", and the
    /// answer is a combination of those rather than any one of them.
    state: String,
}

/// Every control matching a role, a name pattern and a state.
///
/// Replaces the `layout | awk` pipeline that every non-trivial question used to
/// need. The filters are the ones that were actually reassembled by hand, over
/// and over: which buttons are off screen, what is painting at 0x0, what does
/// this surface contain.
#[expect(
    clippy::too_many_arguments,
    reason = "each one is a filter the caller asks for by name"
)]
async fn find(
    client: &mut Client,
    pattern: &str,
    roles: &[String],
    visible: bool,
    hidden: bool,
    painted: bool,
    offscreen_only: bool,
    disabled: bool,
    count_only: bool,
    limit: Option<usize>,
) -> Result<()> {
    let (snapshot, elapsed) = inspect(client).await?;
    let viewport = viewport_of(&snapshot);

    let rows: Vec<FoundRow> = snapshot
        .nodes
        .iter()
        .filter(|node| roles.is_empty() || roles.iter().any(|role| role == &node.role))
        .filter(|node| name_matches(&node.name, pattern))
        .filter(|node| !visible || node.visible)
        .filter(|node| !hidden || !node.visible)
        .filter(|node| !disabled || !node.enabled)
        .filter(|node| {
            !painted
                || node
                    .bounds
                    .is_some_and(|bounds| bounds[2] > 0.0 && bounds[3] > 0.0)
        })
        .filter(|node| {
            !offscreen_only
                || node
                    .bounds
                    .is_some_and(|bounds| offscreen(bounds, viewport))
        })
        .map(|node| {
            let bounds = node.bounds.unwrap_or([0.0; 4]);
            let mut state: Vec<&str> = Vec::new();
            state.push(if node.visible { "visible" } else { "hidden" });
            if !node.enabled {
                state.push("disabled");
            }
            if bounds[2] <= 0.0 || bounds[3] <= 0.0 {
                state.push("0x0");
            } else if offscreen(bounds, viewport) {
                state.push("offscreen");
            }
            // Rounded to a tenth. Layout arrives as f64 and prints as
            // `20.8799991607666`, which is six times the tokens of `20.9` and
            // tells the reader nothing a control's geometry depends on.
            let round = |value: f64| (value * 10.0).round() / 10.0;
            FoundRow {
                id: node.id,
                role: node.role.clone(),
                name: node.name.clone(),
                x: round(bounds[0]),
                y: round(bounds[1]),
                w: round(bounds[2]),
                h: round(bounds[3]),
                state: state.join(","),
            }
        })
        .collect();

    let matched = rows.len();
    let shown: Vec<FoundRow> = match limit {
        Some(limit) => rows.into_iter().take(limit).collect(),
        None => rows,
    };

    #[derive(serde::Serialize)]
    struct Found {
        matched: usize,
        of: usize,
        inspect_ms: f64,
        controls: Vec<FoundRow>,
    }

    let report = Found {
        matched,
        of: snapshot.nodes.len(),
        inspect_ms: (elapsed * 10.0).round() / 10.0,
        controls: if count_only { Vec::new() } else { shown },
    };
    println!(
        "{}",
        toon_format::encode_default(&report).map_err(|error| eyre!(error.to_string()))?
    );
    Ok(())
}

/// Glob-style name matching.
///
/// `chat*` for a prefix, `*close*` for anywhere, `*` for everything. A pattern
/// with no `*` is a substring, because that is how a control is remembered:
/// asking for `Rename` should find `Rename project` without ceremony.
fn name_matches(name: &str, pattern: &str) -> bool {
    let (name, pattern) = (name.to_lowercase(), pattern.to_lowercase());
    if pattern == "*" {
        return true;
    }
    let Some(stripped) = pattern.strip_prefix('*') else {
        return match pattern.strip_suffix('*') {
            Some(prefix) => name.starts_with(prefix),
            None => name.contains(&pattern),
        };
    };
    match stripped.strip_suffix('*') {
        Some(middle) => name.contains(middle),
        None => name.ends_with(stripped),
    }
}

/// The viewport, read from the tree rather than assumed.
///
/// Shared by every driving mode so they agree on what is on screen. `sweep` and
/// `cover` already read it this way; `open_named` and `press_named` did not,
/// which is the bug the two helpers below exist to close.
fn viewport_of(snapshot: &AgentSnapshot) -> (f64, f64) {
    /*
     * The window, not `main`.
     *
     * `main` starts below the title and tab bar - measured here at y=58 in a
     * 900px window - so treating its top as the viewport's top declares the tab
     * strip off-screen. That is chrome sitting legitimately *above* the content
     * region, and rejecting it made every surface reached by a tab unnavigable.
     * Taking the top of the window keeps the below-the-fold case, which is what
     * this bound is actually for, without swallowing the header.
     */
    let bottom = snapshot
        .nodes
        .iter()
        .filter(|node| node.role == "main")
        .filter_map(|node| node.bounds)
        .map(|b| b[1] + b[3])
        .fold(f64::MIN, f64::max);
    (0.0, if bottom > f64::MIN { bottom } else { f64::MAX })
}

/// Whether a node's box lies outside the window, so pressing it would land on
/// nothing.
///
/// A tab strip that overflows keeps its buttons at negative x, and the panel's
/// lower sections sit below the fold. Both are in the tree, both report
/// `visible` and `enabled`, and both have a real width and height, so the
/// obvious predicate accepts them and the press goes to a point the window
/// never had. That is why `rename-project-header` reported the application's
/// pencil dead: its project tab sat at x=-742 and the navigation press that was
/// meant to reach the surface silently hit nothing.
fn offscreen(bounds: [f64; 4], viewport: (f64, f64)) -> bool {
    bounds[1] + bounds[3] < viewport.0 || bounds[0] + bounds[2] < 0.0 || bounds[1] > viewport.1
}

/// Find a named semantic control, scrolling it into view when it is off-screen.
///
/// Skipping an off-screen control is honest but useless: an overflowing tab
/// strip parks every project tab at negative x, so a check that navigates to a
/// project could never reach one, and the surface behind it was untestable.
/// `ScrollIntoView` is what a person does by scrolling, so the control is
/// driven where it actually is rather than at a point outside the window.
///
/// Returns the node id and its box *after* any scroll, because the coordinates
/// the caller presses at must be the ones the renderer just settled on.
async fn locate_control(
    client: &mut Client,
    want: &str,
    roles: &[&str],
) -> Result<(u64, [f64; 4])> {
    let wanted = want.to_lowercase();
    /*
     * An on-screen match wins over an earlier off-screen one.
     *
     * Taking the first match in tree order and then asking whether it is
     * on-screen is wrong when a name matches more than once, which is normal: a
     * document appears in the tab strip, in the root list and in its own header.
     * Doing that can report the root unreachable while a pressable copy is on
     * screen because an overflowed copy came first in the tree.
     */
    let pick = |snapshot: &AgentSnapshot, viewport: (f64, f64)| -> Option<(u64, [f64; 4])> {
        let modal_scope: HashSet<u64> = reach::dismissers(&snapshot.nodes)
            .first()
            .map(|(id, _)| reach::enclosing_dialog(&snapshot.nodes, *id))
            .unwrap_or_default()
            .into_iter()
            .collect();
        let surface_scope: HashSet<u64> = reach::surfaces()
            .iter()
            .find(|surface| reach::on_surface(&snapshot.nodes, surface))
            .map(|surface| reach::on_surface_subtree(&snapshot.nodes, surface))
            .unwrap_or_default()
            .into_iter()
            .collect();
        let candidates: Vec<_> = snapshot
            .nodes
            .iter()
            .filter(|n| {
                roles.is_empty() && reach::interactive(n) || roles.contains(&n.role.as_str())
            })
            .filter(|n| n.name.to_lowercase().contains(&wanted))
            // Geometry is authoritative here. Some renderer-backed controls
            // report `visible=false` while retaining a real painted box; the
            // QA verdict uses the same rule. Truly hidden retained nodes are
            // still rejected below because their box is 0x0.
            .filter(|n| n.enabled)
            .filter_map(|node| {
                node.bounds
                    .filter(|bounds| bounds[2] > 0.0 && bounds[3] > 0.0)
                    .map(|bounds| (node, bounds))
            })
            .collect();

        // Prefer the modal in front, then the active surface, then global
        // chrome. Retained panes can keep enabled, painted controls with the
        // same name; tree order is not a statement about which one owns the
        // interaction the caller can currently see.
        for scope in [&modal_scope, &surface_scope] {
            if let Some((node, bounds)) = candidates
                .iter()
                .find(|(node, bounds)| scope.contains(&node.id) && !offscreen(*bounds, viewport))
            {
                return Some((node.id, *bounds));
            }
        }
        if let Some((node, bounds)) = candidates
            .iter()
            .find(|(_, bounds)| !offscreen(*bounds, viewport))
        {
            return Some((node.id, *bounds));
        }

        let mut fallback = None;
        for (node, bounds) in candidates {
            /*
             * Of the off-screen matches, prefer one `ScrollIntoView` can
             * actually recover.
             *
             * It scrolls vertically. A control below the fold comes back; one
             * pushed off the side by an overflowing strip does not, and
             * `reveal` on such a node reports `moved 0.0`. Taking the first
             * match in tree order picked exactly that unreachable case: a
             * project name matches both its tab, parked at x=-742, and its row
             * in the list at y=2057, and only the second could be reached.
             *
             * This is a consolation prize, not a fix. The protocol's `Pointer`
             * takes x and y where `Click` takes a node id, so a real
             * press-and-release has to compute a coordinate at all, and a
             * control the window cannot show has no honest one. `ps-qa find
             * --offscreen` reports which controls are in that state.
             */
            let recoverable = bounds[0] + bounds[2] > 0.0;
            if recoverable && !fallback.is_some_and(|(_, b): (u64, [f64; 4])| b[0] + b[2] > 0.0) {
                fallback = Some((node.id, bounds));
            } else {
                fallback.get_or_insert((node.id, bounds));
            }
        }
        fallback
    };

    let (snapshot, _) = inspect(client).await?;
    let viewport = viewport_of(&snapshot);
    let Some((id, bounds)) = pick(&snapshot, viewport) else {
        bail!("no visible, enabled, sized semantic control matching it");
    };
    if !offscreen(bounds, viewport) {
        return Ok((id, bounds));
    }

    if cli::trace() {
        println!("        {want:?} is off-screen at {bounds:?}, scrolling it in");
    }
    let mut target = (id, bounds);
    let mut latest = snapshot;
    for _ in 0..4 {
        for node_id in reach::reveal_chain(&latest.nodes, target.0) {
            client
                .agent(&AgentControlRequest::Act(AgentAction::ScrollIntoView {
                    node_id,
                }))
                .await?;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;

        // Re-inspect rather than reusing the old box: the scroll moved it, and
        // pressing at where it used to be is the bug this exists to fix. Nested
        // scrollers may need more than one semantic reveal: the first exposes
        // the node inside its local list, the next exposes that list in the
        // outer surface.
        let (settled, _) = inspect(client).await?;
        let viewport = viewport_of(&settled);
        let Some(found) = pick(&settled, viewport) else {
            bail!("no visible, enabled, sized semantic control matching it");
        };
        target = found;
        if !offscreen(target.1, viewport) {
            return Ok(target);
        }

        // `scrollIntoView` can finish the innermost list while its containing
        // panel is still below the viewport. Move the outermost semantic
        // ancestor's nearest scroller by exactly the remaining vertical gap;
        // the node id still selects the scroll context, never a screen point.
        let delta_y = if target.1[1] > viewport.1 {
            target.1[1] + target.1[3] - viewport.1 + 16.0
        } else if target.1[1] + target.1[3] < viewport.0 {
            target.1[1] - viewport.0 - 16.0
        } else {
            0.0
        };
        if delta_y != 0.0
            && let Some(node_id) = reach::reveal_chain(&settled.nodes, target.0).first()
        {
            client
                .agent(&AgentControlRequest::Act(AgentAction::ScrollBy {
                    node_id: *node_id,
                    delta_x: 0.0,
                    delta_y,
                }))
                .await?;
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        latest = settled;
    }
    bail!(
        "{want:?} is still off-screen at {:?} after four semantic reveal attempts",
        target.1
    )
}

async fn locate_button(client: &mut Client, want: &str) -> Result<(u64, [f64; 4])> {
    locate_control(client, want, &["button"]).await
}

/// Double click the first visible match, for reaching a surface.
///
/// A document row may fold on a single click and open on a double; two separate
/// `press` calls are not a double click: each round-trips through the
/// inspector, so they land hundreds of milliseconds apart and the row folds
/// there and back. Navigation that used single presses appeared to work only
/// when the surface happened to be open already, which is why checks failed
/// with "no visible, enabled, sized button" for controls one gesture away.
async fn open_named(client: &mut Client, want: &str) -> Result<()> {
    let (id, _) = locate_button(client, want).await?;
    if cli::trace() {
        println!("        opening {want:?} (id {id})");
    }
    client
        .agent(&AgentControlRequest::Act(AgentAction::DoubleClick {
            node_id: id,
        }))
        .await?;
    Ok(())
}

/// Send a coordinate-pointer press to the first visible match, when requested.
///
/// This is an explicit generic hit-testing diagnostic. Application suites use
/// the node-addressed default and opt into this only when pointer hit-testing
/// itself is the behavior under test.
async fn press_named(client: &mut Client, want: &str) -> Result<()> {
    /*
     * Buttons only, and on screen - see `locate_button`.
     *
     * A control and the thing it opens may share an accessible name, so a
     * name-only match can select the output instead of the activator.
     * Restricting this explicit diagnostic to buttons keeps the target clear.
     */
    let (id, b) = locate_button(client, want).await?;
    let (x, y) = (b[0] + b[2] / 2.0, b[1] + b[3] / 2.0);
    if cli::trace() {
        println!("        pressing {want:?} (id {id}) at {x:.0},{y:.0}");
    }
    for phase in [PointerPhase::Move, PointerPhase::Down, PointerPhase::Up] {
        let answer = client
            .agent(&AgentControlRequest::Act(AgentAction::Input(
                InputCommand::Pointer {
                    phase,
                    x,
                    y,
                    button: 0,
                    modifiers: Modifiers::default(),
                },
            )))
            .await?;
        if let DebugResponse::Error(error) = answer.response {
            bail!("{} ({})", error.message, error.code);
        }
    }
    Ok(())
}

/// `click_named` without the metrics report, for the QA runner's inner loop.
async fn click_named_quiet(client: &mut Client, want: &str) -> Result<u64> {
    // Resolve through the same painted, on-screen semantic path as navigation.
    // `visible` alone is insufficient: retained subtrees can expose an old
    // semantic node at 0x0, and activating that id reports a working control
    // dead while its current painted replacement remains untouched.
    let (target_id, _) = locate_control(client, want, &[]).await?;
    if cli::trace() {
        println!("        activating {want:?} (id {target_id})");
    }
    client
        .agent(&AgentControlRequest::Act(AgentAction::Click {
            node_id: target_id,
        }))
        .await?;
    Ok(target_id)
}

/// What a captured frame contains, in the terms a person would use.
///
/// "Did it draw" is not answerable from a pixel count alone: an icon filled
/// black on a near-black surface is fully opaque and completely invisible, and
/// that exact failure shipped. What separates ink from background is contrast,
/// so that is what this measures.
struct Ink {
    /// Pixels that differ enough from the most common colour to be seen.
    visible: usize,
    total: usize,
    /// The colour occupying the most pixels, taken as the background.
    background: (u8, u8, u8),
}

impl Ink {
    fn fraction(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.visible as f64 / self.total as f64
        }
    }
}

/// Measure the visible ink in a captured frame.
///
/// The background is discovered rather than assumed, so this works against any
/// app's surface colour without being told what it is. Contrast is relative
/// luminance, because that is what decides whether a person can see the mark:
/// raw channel distance calls black-on-near-black "different" while being the
/// one case worth catching.
fn measure_ink(image: &CapturedImage) -> Result<Ink> {
    use base64::Engine as _;

    let rgba = base64::engine::general_purpose::STANDARD
        .decode(&image.rgba_base64)
        .map_err(|error| eyre::eyre!("the capture was not valid base64: {error}"))?;
    let expected = (image.width as usize) * (image.height as usize) * 4;
    if rgba.len() != expected {
        bail!(
            "capture is {} bytes, expected {expected} for {}x{}",
            rgba.len(),
            image.width,
            image.height
        );
    }

    let mut histogram: HashMap<(u8, u8, u8), usize> = HashMap::new();
    // `as_chunks::<4>()` rather than `chunks_exact(4)`: the width is a constant,
    // so this hands back `[u8; 4]` and the indexing below is bounds-checked at
    // compile time instead of per pixel.
    for pixel in rgba.as_chunks::<4>().0 {
        *histogram.entry((pixel[0], pixel[1], pixel[2])).or_default() += 1;
    }
    let background = histogram
        .iter()
        .max_by_key(|(_, count)| **count)
        .map(|(colour, _)| *colour)
        .unwrap_or((0, 0, 0));

    let luminance = |(r, g, b): (u8, u8, u8)| {
        0.299 * f64::from(r) + 0.587 * f64::from(g) + 0.114 * f64::from(b)
    };
    let background_luminance = luminance(background);

    let visible = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| {
            // Transparent pixels are not ink whatever their colour.
            if pixel[3] < 32 {
                return false;
            }
            (luminance((pixel[0], pixel[1], pixel[2])) - background_luminance).abs() > 24.0
        })
        .count();

    Ok(Ink {
        visible,
        total: (image.width as usize) * (image.height as usize),
        background,
    })
}

/// Ask the app for a frame: the whole window, or one named node.
async fn capture(client: &mut Client, want: &str, scale: f32) -> Result<()> {
    let node_id = if want.is_empty() {
        None
    } else {
        let (snapshot, _) = inspect(client).await?;
        let node = snapshot
            .nodes
            .iter()
            .filter(|node| node.name.contains(want) && node.visible)
            .filter_map(|node| node.bounds.map(|bounds| (node, bounds)))
            .filter(|(_, bounds)| bounds[2] > 0.0 && bounds[3] > 0.0)
            .max_by(|a, b| {
                (a.1[2] * a.1[3])
                    .partial_cmp(&(b.1[2] * b.1[3]))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(node, _)| node);
        let Some(node) = node else {
            bail!("no visible node with a box whose name contains {want:?}");
        };
        println!(
            "capturing {} role={} name={}",
            node.id,
            node.role,
            report::py_repr(&node.name.chars().take(50).collect::<String>())
        );
        Some(node.id)
    };

    let answer = client
        .diagnostics(&DiagnosticsRequest::Capture(CaptureRequest {
            node_id,
            scale,
        }))
        .await?;
    let image = match answer.response {
        DebugResponse::Captured(image) => image,
        DebugResponse::Error(error) => bail!("capture refused: {} ({})", error.message, error.code),
        other => bail!("asked for a capture, got {other:?}"),
    };

    let ink = measure_ink(&image)?;
    println!(
        "{}x{} at {scale}x, background #{:02x}{:02x}{:02x}",
        image.width, image.height, ink.background.0, ink.background.1, ink.background.2
    );
    println!(
        "visible ink: {} of {} pixels ({:.2}%)",
        ink.visible,
        ink.total,
        ink.fraction() * 100.0
    );
    if ink.visible == 0 {
        println!("nothing was drawn: every pixel is the background colour");
    }
    Ok(())
}

/// Audit every button in the running application.
///
/// Reads the renderer's own paint output rather than capturing each control.
/// The paint snapshot reports, per node, the style the renderer resolved and
/// the box it drew into, which answers the same question a capture does and
/// answers it for the whole window in one call.
///
/// Capturing per button was tried first and was worse in both directions. It
/// took 19 seconds against well under one, and it reported false faults: a crop
/// taken from a full-document paint cannot see content a clipping scroller drew
/// into its own layer, so three `Edit` buttons were flagged that the paint
/// output proved were drawn, with colours identical to their working siblings.
async fn run_audit(client: &mut Client, family: Option<&str>) -> Result<usize> {
    use audit::{Audited, Verdict};

    let (snapshot, _) = inspect(client).await?;

    /*
     * The viewport, read from the tree rather than assumed.
     *
     * A control below the fold is clipped, not broken. Lower section headers
     * can sit outside a short window and draw perfectly once
     * scrolled to, so a hardcoded bound reported them as faults. Reading the
     * `main` box also keeps this right when the window is resized.
     */
    let viewport = snapshot
        .nodes
        .iter()
        .filter(|node| node.role == "main")
        .filter_map(|node| node.bounds)
        .map(|b| (b[1], b[1] + b[3]))
        .max_by(|a, b| {
            (a.1 - a.0)
                .partial_cmp(&(b.1 - b.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or((0.0, f64::MAX));

    let painted = painted_nodes(client).await?;

    let mut rows: Vec<Audited> = Vec::new();
    for node in audit::buttons(&snapshot.nodes) {
        let family_name = audit::family_of(&node.name);
        if family.is_some_and(|want| family_name != want) {
            continue;
        }
        let (width, height) = node.bounds.map(|b| (b[2], b[3])).unwrap_or((0.0, 0.0));

        let verdict = if !node.visible {
            Verdict::Hidden
        } else if width <= 0.0 || height <= 0.0 {
            Verdict::NoBox
        } else if node
            .bounds
            .is_some_and(|b| b[1] + b[3] < viewport.0 || b[0] + b[2] < 0.0 || b[1] > viewport.1)
        {
            Verdict::Offscreen
        } else if painted.contains(&node.id) {
            Verdict::Drawn
        } else {
            Verdict::Blank
        };

        rows.push(Audited {
            name: node.name.clone(),
            family: family_name,
            width,
            height,
            verdict,
        });
    }

    if rows.is_empty() {
        bail!("no buttons matched {family:?}");
    }
    println!("auditing {} buttons in the running app\n", rows.len());

    let faults: Vec<&Audited> = rows.iter().filter(|row| row.verdict.is_fault()).collect();
    if faults.is_empty() {
        println!("no faults: every visible button was painted");
    } else {
        println!("{} button(s) nobody can see:\n", faults.len());
        for row in &faults {
            println!(
                "  {:<8} {:<52} {:.0}x{:.0}",
                row.verdict.label(),
                row.name.chars().take(52).collect::<String>(),
                row.width,
                row.height
            );
        }
        println!();
    }

    let mut families: Vec<_> = audit::by_family(&rows).into_iter().collect();
    families.sort_by_key(|(name, _)| *name);
    for (name, (passed, total)) in families {
        let mark = if passed == total { " " } else { "!" };
        println!("{mark} {name:<12} {passed}/{total}");
    }
    println!("\n{} audited, {} faults", rows.len(), faults.len());
    Ok(faults.len())
}

/// The nodes the renderer resolved and drew, from one paint snapshot.
///
/// A button absent from this was not painted, which is what "a person cannot
/// see it" means. Asked once for the whole window rather than per control.
async fn painted_nodes(client: &mut Client) -> Result<HashSet<u64>> {
    let answer = client
        .diagnostics(&DiagnosticsRequest::Snapshot(SnapshotRequest {
            include_dom: false,
            include_layout: false,
            include_computed_style: true,
        }))
        .await?;
    let DebugResponse::Snapshot(snapshot) = answer.response else {
        bail!("asked for a paint snapshot, got {:?}", answer.response);
    };
    Ok(snapshot
        .computed_style
        .as_ref()
        .and_then(|value| value.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("nodeId")?.as_u64())
                .collect()
        })
        .unwrap_or_default())
}

/// Click every button and report the ones that did not act.
///
/// The tree is re-read after each click rather than once at the end, because a
/// click changes what is on screen and a stale node id is a click on nothing.
/// Buttons are addressed by name for the same reason: ids do not survive the
/// re-render that a working button causes.
async fn run_sweep(client: &mut Client, family: Option<&str>) -> Result<usize> {
    let (snapshot, _) = inspect(client).await?;
    let viewport = snapshot
        .nodes
        .iter()
        .filter(|node| node.role == "main")
        .filter_map(|node| node.bounds)
        .map(|b| (b[1], b[1] + b[3]))
        .max_by(|a, b| {
            (a.1 - a.0)
                .partial_cmp(&(b.1 - b.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or((0.0, f64::MAX));
    let planned = sweep::cases(
        &snapshot.nodes,
        family,
        audit::family_of,
        reach::is_inert_control,
    );
    if planned.is_empty() {
        bail!("no clickable buttons matched {family:?}");
    }
    println!("clicking {} buttons\n", planned.len());

    let mut outcomes: Vec<sweep::Outcome> = Vec::new();
    for case in planned {
        let (before, _) = inspect(client).await?;

        /*
         * Re-resolved by id, freshly, every time.
         *
         * Clicking by name cannot work here: fifteen task-log rows are all
         * called "Show the whole command", so after the first click the lookup
         * is ambiguous and every later one reports a failure that is the
         * harness's, not the application's. That produced 24 false failures in
         * the first run and buried whatever was real.
         *
         * The id is re-checked against the current tree rather than trusted
         * from the plan, because a working button re-renders its own row and a
         * stale id is a click on nothing.
         */
        let Some(node) = before.nodes.iter().find(|node| node.id == case.id) else {
            // Gone since the plan was made, which a working button often
            // causes: closing one tab removes the close buttons of its
            // neighbours. Not a failure.
            continue;
        };
        if !node.visible || !node.enabled {
            continue;
        }
        /*
         * Off the viewport is not clickable, and clicking it anyway tests the
         * harness rather than the application. A transcript keeps hundreds of
         * controls at negative coordinates and the panel's lower sections sit
         * below the fold; both reported as failures until they were skipped.
         */
        if node
            .bounds
            .is_some_and(|b| b[1] + b[3] < viewport.0 || b[0] + b[2] < 0.0 || b[1] > viewport.1)
        {
            continue;
        }

        if let Err(error) = click_by_id(client, case.id).await {
            outcomes.push(sweep::Outcome {
                case,
                failure: Some(format!("could not be clicked: {error}")),
            });
            continue;
        }
        // Long enough for a synchronous handler and its re-render. A backend
        // round trip is slower, and a button that only fails under that delay
        // is reported rather than waited for: a person sees the same thing.
        tokio::time::sleep(Duration::from_millis(250)).await;

        let (after, _) = inspect(client).await?;
        let failure = sweep::judge(&case, &before.nodes, &after.nodes);
        outcomes.push(sweep::Outcome { case, failure });
    }

    let failures: Vec<&sweep::Outcome> = outcomes.iter().filter(|o| o.failure.is_some()).collect();
    if failures.is_empty() {
        println!("every button acted");
    } else {
        println!("{} button(s) did not act:\n", failures.len());
        for outcome in &failures {
            println!(
                "  {:<48} {}",
                outcome.case.name.chars().take(48).collect::<String>(),
                outcome.failure.as_deref().unwrap_or("")
            );
        }
        println!();
    }

    let mut by_family: HashMap<&'static str, (usize, usize)> = HashMap::new();
    for outcome in &outcomes {
        let entry = by_family.entry(outcome.case.family).or_insert((0, 0));
        entry.1 += 1;
        if outcome.failure.is_none() {
            entry.0 += 1;
        }
    }
    let mut families: Vec<_> = by_family.into_iter().collect();
    families.sort_by_key(|(name, _)| *name);
    for (name, (passed, total)) in families {
        let mark = if passed == total { " " } else { "!" };
        println!("{mark} {name:<12} {passed}/{total}");
    }
    println!(
        "\n{} clicked, {} did not act",
        outcomes.len(),
        failures.len()
    );
    Ok(failures.len())
}

/// Open every collapsed section on the current surface.
///
/// Repeated until nothing more opens, because expanding one section reveals
/// disclosure controls inside it: a collection may hold a row per record, each with
/// its own. A single pass stops one level short of the controls that matter.
async fn expand_everything(client: &mut Client, surface: &reach::Surface) -> Result<usize> {
    let mut opened = 0;
    // Bounded: a disclosure that reports "Expand" after being expanded would
    // otherwise spin here forever, and that is a bug worth finishing the run to
    // report rather than hanging on.
    for _ in 0..6 {
        let (tree, _) = inspect(client).await?;
        // This surface's disclosures only: a retained pane keeps its own, and
        // pressing one of those navigates out of the surface being planned.
        let mine: std::collections::HashSet<u64> = reach::on_surface_subtree(&tree.nodes, surface)
            .into_iter()
            .collect();
        let mut todo: Vec<String> = reach::expanders(&tree.nodes)
            .into_iter()
            .filter(|(id, _)| mine.contains(id))
            .map(|(_, name)| name)
            .collect();
        // Retained panes can expose dozens of identical disclosure names. One
        // semantic activation on the active surface is the user action; driving
        // every retained node toggles unrelated panes and can leave the current
        // one exactly where it started.
        todo.sort();
        todo.dedup();
        if todo.is_empty() {
            break;
        }
        for name in todo {
            if click_named_quiet(client, &name).await.is_ok() {
                opened += 1;
                tokio::time::sleep(Duration::from_millis(80)).await;
            }
        }
    }
    Ok(opened)
}

/// Reveal lazily mounted content on the active surface without guessing a
/// coordinate or knowing an application's section names.
///
/// Some long settings pages render their section shells first and mount each
/// body only when scrolling approaches it. If a check names a control in a
/// deferred body, that control cannot be addressed yet. The deepest rendered
/// node is the semantic equivalent of dragging the page toward its end; after
/// each node-ID reveal the tree is inspected again and the requested control
/// wins as soon as it has a real box.
async fn reveal_deferred_content(
    client: &mut Client,
    surface: &reach::Surface,
    want: &str,
) -> Result<usize> {
    let materialized = materialize_deferred_content(client, surface).await?;
    if materialized > 0 {
        let (snapshot, _) = inspect(client).await?;
        if painted_named(&snapshot.nodes, want) {
            return Ok(materialized);
        }
    }
    for step in 0..8 {
        let (snapshot, _) = inspect(client).await?;
        if painted_named(&snapshot.nodes, want) {
            return Ok(step);
        }
        let scope: HashSet<u64> = reach::on_surface_subtree(&snapshot.nodes, surface)
            .into_iter()
            .collect();
        let Some(target) = snapshot
            .nodes
            .iter()
            .filter(|node| scope.contains(&node.id))
            .filter_map(|node| {
                node.bounds
                    .filter(|bounds| bounds[2] > 0.0 && bounds[3] > 0.0)
                    .map(|bounds| (node.id, bounds[1] + bounds[3]))
            })
            .max_by(|left, right| {
                left.1
                    .partial_cmp(&right.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        else {
            return Ok(step);
        };
        client
            .agent(&AgentControlRequest::Act(AgentAction::ScrollIntoView {
                node_id: target.0,
            }))
            .await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    Ok(8)
}

/// Ask an application-declared search field to mount all deferred rows, then
/// restore the empty query before the audit reads or activates them.
async fn materialize_deferred_content(
    client: &mut Client,
    surface: &reach::Surface,
) -> Result<usize> {
    let Some(field) = surface.reveal_with.as_deref() else {
        return Ok(0);
    };
    type_text(client, field, "ps-qa reveal deferred content").await?;
    type_text(client, field, "").await?;
    Ok(2)
}

/// Hover every row on the surface, so hover-revealed controls enter the tree.
///
/// The row controls users asked about - rename, delete, pin - do not exist
/// until `pointerenter`. Hovering is what puts them in the tree at all, so this
/// runs before the plan is made rather than per-click.
async fn hover_all_rows(client: &mut Client) -> Result<usize> {
    let (tree, _) = inspect(client).await?;
    // The window band, taken from the largest `main`, so rows scrolled far off
    // the top of a transcript are not hovered at negative coordinates.
    let window = tree
        .nodes
        .iter()
        .filter(|node| node.role == "main")
        .filter_map(|node| node.bounds)
        .map(|b| (b[1], b[1] + b[3]))
        .max_by(|a, b| {
            (a.1 - a.0)
                .partial_cmp(&(b.1 - b.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or((0.0, 4000.0));
    let mut revealed = 0;
    for node_id in reach::hover_row_ids(&tree.nodes, "listitem", window) {
        if client
            .agent(&AgentControlRequest::Act(AgentAction::Hover { node_id }))
            .await
            .is_ok()
        {
            revealed += 1;
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }
    Ok(revealed)
}

/// Inventory semantic reachability across every configured application
/// surface without pressing the controls being counted.
///
/// `cover` answers a different and much more expensive question: it activates
/// every eligible button and performs multiple full-tree reads per button.
/// An agent asking which components are unreachable should not pay that cost or
/// mutate the profile merely to obtain counts. Navigation, disclosure expansion
/// and hover are still necessary preconditions, and all three are node-id
/// actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InventoryClass {
    Manual,
    Isolated,
    Anonymous,
    Unreachable,
    Disabled,
    Reachable,
}

fn inventory_class(node: &SemanticNode, manual: bool, isolated: bool) -> InventoryClass {
    if manual {
        InventoryClass::Manual
    } else if isolated {
        InventoryClass::Isolated
    } else if node.name.trim().is_empty() {
        InventoryClass::Anonymous
    } else if !reach::onscreen(node) {
        InventoryClass::Unreachable
    } else if !node.enabled {
        InventoryClass::Disabled
    } else {
        InventoryClass::Reachable
    }
}

/// Whether a selector from an outcome check addresses this exact semantic
/// control shape.
///
/// `role:name` is the precise spelling used by checks when a button and the
/// field it opens share a name. Everything else follows the same substring or
/// glob matching as the live driver, so coverage cannot claim a selector the
/// runner itself would never resolve.
fn selector_matches_node(node: &SemanticNode, selector: &str) -> bool {
    if let Some((role, name)) = selector.split_once(':')
        && role == node.role
    {
        return name_matches(&node.name, name);
    }
    name_matches(&node.name, selector)
}

/// Named outcomes that drive or assert this control.
///
/// A navigation opener and a hover are actions too: their check fails when
/// the control cannot perform them. `subject` and `compare` are observed
/// outcomes. Merely appearing in the inventory is intentionally absent here.
fn outcome_check_ids(node: &SemanticNode, checks: &[qa::Check]) -> Vec<String> {
    checks
        .iter()
        .filter(|check| {
            [
                check.open.as_deref(),
                check.hover.as_deref(),
                check.click.as_deref(),
                check.type_into.as_deref(),
                check.key_on.as_deref(),
                check.compare.as_deref(),
                Some(check.subject.as_str()),
            ]
            .into_iter()
            .flatten()
            .chain(check.covers.iter().map(String::as_str))
            .any(|selector| selector_matches_node(node, selector))
        })
        .map(|check| check.id.clone())
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
struct SavedControl {
    surface: String,
    role: String,
    name: String,
    classification: String,
}

/// Parse one row from the TOON table emitted by `inventory`.
///
/// TOON quotes fields containing commas. This parser needs only the first five
/// scalar columns and deliberately ignores any later columns added by a newer
/// harness, so an old CI artifact remains useful after the report grows.
fn saved_control_row(line: &str) -> Option<SavedControl> {
    let line = line.strip_prefix("  ")?;
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in line.chars() {
        if escaped {
            field.push(character);
            escaped = false;
        } else if quoted && character == '\\' {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == ',' && !quoted {
            fields.push(std::mem::take(&mut field));
        } else {
            field.push(character);
        }
    }
    fields.push(field);
    (fields.len() >= 5).then(|| SavedControl {
        surface: fields[0].trim().to_owned(),
        role: fields[2].trim().to_owned(),
        name: fields[3].trim().to_owned(),
        classification: fields[4].trim().to_owned(),
    })
}

fn saved_controls(report: &str) -> Result<Vec<SavedControl>, String> {
    let mut in_controls = false;
    let mut controls = Vec::new();
    for line in report.lines() {
        if line.starts_with("controls[") {
            in_controls = true;
            continue;
        }
        if in_controls && let Some(control) = saved_control_row(line) {
            controls.push(control);
        }
    }
    if !in_controls {
        return Err("inventory report has no controls table".into());
    }
    Ok(controls)
}

fn reconcile_inventory(
    inventory: &std::path::Path,
    checks_dir: Option<&std::path::Path>,
) -> Result<usize> {
    #[derive(serde::Serialize)]
    struct MissingRow {
        surface: String,
        role: String,
        name: String,
        classification: String,
        checks: Vec<String>,
    }

    #[derive(serde::Serialize)]
    struct ReconcileReport {
        components: usize,
        outcome_declared: usize,
        excluded_manual: usize,
        failed_existing: usize,
        unverified: usize,
        controls: Vec<MissingRow>,
    }

    let input = std::fs::read_to_string(inventory)?;
    let controls = saved_controls(&input).map_err(eyre::Report::msg)?;
    let checks = qa::checks(checks_dir).map_err(eyre::Report::msg)?;
    let mut outcome_declared = 0;
    let mut excluded_manual = 0;
    let mut failed_existing = 0;
    let mut missing = Vec::new();

    for control in &controls {
        let node = SemanticNode {
            id: 0,
            parent: None,
            role: control.role.clone(),
            name: control.name.clone(),
            value: None,
            enabled: !control.classification.contains("disabled"),
            visible: !control.classification.contains("unreachable"),
            selected: false,
            bounds: Some([0.0, 0.0, 1.0, 1.0]),
        };
        let matched = outcome_check_ids(&node, &checks);
        if control.classification == "excluded-manual" {
            excluded_manual += 1;
        } else if control.classification.starts_with("failed-") {
            failed_existing += 1;
            missing.push(MissingRow {
                surface: control.surface.clone(),
                role: control.role.clone(),
                name: control.name.clone(),
                classification: control.classification.clone(),
                checks: matched,
            });
        } else if control.classification.contains("isolated") || matched.is_empty() {
            missing.push(MissingRow {
                surface: control.surface.clone(),
                role: control.role.clone(),
                name: control.name.clone(),
                classification: if control.classification.contains("isolated") {
                    "isolated-unverified".into()
                } else if control.classification.contains("disabled") {
                    "state-disabled-unverified".into()
                } else {
                    "outcome-unverified".into()
                },
                checks: matched,
            });
        } else {
            outcome_declared += 1;
        }
    }

    let unverified = missing
        .iter()
        .filter(|row| !row.classification.starts_with("failed-"))
        .count();
    let report = ReconcileReport {
        components: controls.len(),
        outcome_declared,
        excluded_manual,
        failed_existing,
        unverified,
        controls: missing,
    };
    println!(
        "{}",
        toon_format::encode_default(&report).map_err(|error| eyre!(error.to_string()))?
    );
    Ok(report.failed_existing + report.unverified)
}

async fn run_inventory(
    client: &mut Client,
    only: Option<&str>,
    require_outcomes: bool,
) -> Result<usize> {
    #[derive(serde::Serialize)]
    struct SurfaceRow {
        surface: String,
        opened: bool,
        components: usize,
        reachable: usize,
        unreachable: usize,
        anonymous: usize,
        disabled: usize,
        manual: usize,
        isolated: usize,
        outcome_declared: usize,
        unverified: usize,
        sections_opened: usize,
        rows_hovered: usize,
    }

    #[derive(serde::Serialize)]
    struct ControlRow {
        surface: String,
        id: u64,
        role: String,
        name: String,
        classification: String,
        reason: String,
        checks: Vec<String>,
    }

    #[derive(serde::Serialize)]
    struct RoleRow {
        role: String,
        components: usize,
        reachable: usize,
        unreachable: usize,
        anonymous: usize,
        disabled: usize,
        manual: usize,
        isolated: usize,
        outcome_declared: usize,
        unverified: usize,
    }

    #[derive(serde::Serialize)]
    struct InventoryReport {
        components: usize,
        reachable: usize,
        unreachable: usize,
        anonymous: usize,
        disabled: usize,
        manual: usize,
        isolated: usize,
        outcome_declared: usize,
        unverified: usize,
        surfaces: Vec<SurfaceRow>,
        roles: Vec<RoleRow>,
        controls: Vec<ControlRow>,
    }

    let checks = if std::path::Path::new("tests/ps-qa").is_dir() {
        qa::checks(None).map_err(eyre::Report::msg)?
    } else {
        Vec::new()
    };
    let mut rows = Vec::new();
    let mut controls = Vec::new();
    let mut role_counts: std::collections::BTreeMap<String, [usize; 9]> =
        std::collections::BTreeMap::new();
    for surface in reach::surfaces() {
        if only.is_some_and(|want| want != surface.name) {
            continue;
        }
        if !open_surface(client, surface).await? {
            rows.push(SurfaceRow {
                surface: surface.name.clone(),
                opened: false,
                components: 0,
                reachable: 0,
                unreachable: 0,
                anonymous: 0,
                disabled: 0,
                manual: 0,
                isolated: 0,
                outcome_declared: 0,
                unverified: 0,
                sections_opened: 0,
                rows_hovered: 0,
            });
            continue;
        }

        let sections_opened = expand_everything(client, surface).await?;
        if !open_surface(client, surface).await? {
            rows.push(SurfaceRow {
                surface: surface.name.clone(),
                opened: false,
                components: 0,
                reachable: 0,
                unreachable: 0,
                anonymous: 0,
                disabled: 0,
                manual: 0,
                isolated: 0,
                outcome_declared: 0,
                unverified: 0,
                sections_opened,
                rows_hovered: 0,
            });
            continue;
        }
        materialize_deferred_content(client, surface).await?;
        let rows_hovered = hover_all_rows(client).await?;
        let (tree, _) = inspect(client).await?;
        let mine: std::collections::HashSet<u64> = reach::on_surface_subtree(&tree.nodes, surface)
            .into_iter()
            .collect();
        let components: Vec<_> = tree
            .nodes
            .iter()
            .filter(|node| reach::interactive(node) && mine.contains(&node.id))
            .collect();
        let classes: Vec<_> = components
            .iter()
            .map(|node| {
                inventory_class(
                    node,
                    reach::requires_manual_release_check(&node.name),
                    reach::requires_isolated_outcome(&node.name),
                )
            })
            .collect();
        let count = |class| classes.iter().filter(|found| **found == class).count();
        let reachable = count(InventoryClass::Reachable);
        let unreachable = count(InventoryClass::Unreachable);
        let anonymous = count(InventoryClass::Anonymous);
        let disabled = count(InventoryClass::Disabled);
        let manual = count(InventoryClass::Manual);
        let isolated = count(InventoryClass::Isolated);
        let declared: Vec<Vec<String>> = components
            .iter()
            .map(|node| outcome_check_ids(node, &checks))
            .collect();
        let outcome_declared = classes
            .iter()
            .zip(&declared)
            .filter(|(class, matches)| {
                matches!(class, InventoryClass::Reachable | InventoryClass::Disabled)
                    && !matches.is_empty()
            })
            .count();
        let unverified = classes
            .iter()
            .zip(&declared)
            .filter(|(class, matches)| match class {
                InventoryClass::Reachable | InventoryClass::Disabled => matches.is_empty(),
                // These must run in disposable processes; declaration in the
                // shared suite cannot turn them green.
                InventoryClass::Isolated => true,
                InventoryClass::Manual
                | InventoryClass::Anonymous
                | InventoryClass::Unreachable => false,
            })
            .count();
        for (node, matched_checks) in components.iter().zip(&declared) {
            let manual = reach::requires_manual_release_check(&node.name);
            let isolated = reach::requires_isolated_outcome(&node.name);
            let class = inventory_class(node, manual, isolated);
            let counts = role_counts.entry(node.role.clone()).or_default();
            counts[0] += 1;
            match class {
                InventoryClass::Reachable => counts[1] += 1,
                InventoryClass::Unreachable => counts[2] += 1,
                InventoryClass::Anonymous => counts[3] += 1,
                InventoryClass::Disabled => counts[4] += 1,
                InventoryClass::Manual => counts[5] += 1,
                InventoryClass::Isolated => counts[6] += 1,
            }
            if !matched_checks.is_empty() {
                counts[7] += 1;
            }
            let is_unverified = match class {
                InventoryClass::Reachable | InventoryClass::Disabled => matched_checks.is_empty(),
                InventoryClass::Isolated => true,
                InventoryClass::Manual
                | InventoryClass::Anonymous
                | InventoryClass::Unreachable => false,
            };
            if is_unverified {
                counts[8] += 1;
            }
            let (classification, reason) = match class {
                InventoryClass::Manual => ("excluded-manual", "native-dialog-or-external"),
                InventoryClass::Isolated => (
                    "isolated-unverified",
                    "requires disposable-process outcome check",
                ),
                InventoryClass::Anonymous => ("failed-anonymous", "no accessible name"),
                InventoryClass::Unreachable if !node.visible => ("failed-reachability", "hidden"),
                InventoryClass::Unreachable => ("failed-reachability", "no-box"),
                InventoryClass::Disabled if matched_checks.is_empty() => {
                    ("state-disabled-unverified", "no outcome check matched")
                }
                InventoryClass::Disabled => {
                    ("outcome-declared-disabled", "matched named outcome check")
                }
                InventoryClass::Reachable if matched_checks.is_empty() => {
                    ("reachable-unverified", "no outcome check matched")
                }
                InventoryClass::Reachable => ("outcome-declared", "matched named outcome check"),
            };
            controls.push(ControlRow {
                surface: surface.name.clone(),
                id: node.id,
                role: node.role.clone(),
                name: node.name.clone(),
                classification: classification.to_owned(),
                reason: reason.to_owned(),
                checks: matched_checks.clone(),
            });
        }
        rows.push(SurfaceRow {
            surface: surface.name.clone(),
            opened: true,
            components: components.len(),
            reachable,
            unreachable,
            anonymous,
            disabled,
            manual,
            isolated,
            outcome_declared,
            unverified,
            sections_opened,
            rows_hovered,
        });
    }

    let report = InventoryReport {
        components: rows.iter().map(|row| row.components).sum(),
        reachable: rows.iter().map(|row| row.reachable).sum(),
        unreachable: rows.iter().map(|row| row.unreachable).sum(),
        anonymous: rows.iter().map(|row| row.anonymous).sum(),
        disabled: rows.iter().map(|row| row.disabled).sum(),
        manual: rows.iter().map(|row| row.manual).sum(),
        isolated: rows.iter().map(|row| row.isolated).sum(),
        outcome_declared: rows.iter().map(|row| row.outcome_declared).sum(),
        unverified: rows.iter().map(|row| row.unverified).sum(),
        surfaces: rows,
        roles: role_counts
            .into_iter()
            .map(|(role, counts)| RoleRow {
                role,
                components: counts[0],
                reachable: counts[1],
                unreachable: counts[2],
                anonymous: counts[3],
                disabled: counts[4],
                manual: counts[5],
                isolated: counts[6],
                outcome_declared: counts[7],
                unverified: counts[8],
            })
            .collect(),
        controls,
    };
    let failures =
        report.unreachable + report.anonymous + usize::from(require_outcomes) * report.unverified;
    println!(
        "{}",
        toon_format::encode_default(&report).map_err(|error| eyre!(error.to_string()))?
    );
    Ok(failures)
}

/// Navigate to a surface, and say whether it opened.
async fn open_surface(client: &mut Client, surface: &reach::Surface) -> Result<bool> {
    if surface.opener.is_empty() {
        return Ok(true);
    }
    // A dynamic document surface has no fixed name to aim at: the profile may be scrubbed,
    // so the tab is found by shape (a tab is the button whose `Close` twin the
    // strip renders beside it) rather than by a string that would differ per
    // profile.
    let opener = if surface.opener == reach::DYNAMIC_DOCUMENT {
        /*
         * Via the configured root surface, because that is where a dynamic
         * document can be opened from on a fresh profile.
         *
         * The strip holds no project tab until one has been opened, and the
         * sweep may reach the document while standing on another surface, where there is
         * no row to press either. Looking from where we happen to be found
         * nothing every time and the pane went unswept on exactly the runs that
         * matter. Cheap when a tab already exists: the lookup prefers it.
         */
        let (here, _) = inspect(client).await?;
        if reach::document_opener(&here.nodes).is_none() {
            if let Some(home) = reach::profile().home_opener.as_deref() {
                let _ = click_named_quiet(client, home).await;
            }
            tokio::time::sleep(Duration::from_millis(600)).await;
        }
        let (tree, _) = inspect(client).await?;
        match reach::document_opener(&tree.nodes) {
            Some(name) => name,
            None => return Ok(false),
        }
    } else {
        surface.opener.to_owned()
    };
    /*
     * Confirmed by looking, not assumed from the click succeeding.
     *
     * A dispatched click is not proof of navigation: a control can acknowledge
     * activation while the current surface remains unchanged. Always confirm
     * the destination marker before attributing controls to that surface.
     */
    /*
     * A dynamic document row uses the profile contract's double-click gesture;
     * a fixed surface opener uses an ordinary semantic click.
     */
    if surface.opener == reach::DYNAMIC_DOCUMENT {
        /*
         * Scrolled into view before it is aimed at.
         *
         * A box is not a position on screen. The first row this picked sat at
         * y=1152 in a 900px window - laid out, `visible`, and below the fold -
         * so the gesture went to a point with nothing under it and the project
         * surface stayed unreachable while the report said only "could not be
         * opened".
         */
        let (tree, _) = inspect(client).await?;
        let Some(id) = tree
            .nodes
            .iter()
            .find(|n| n.name == opener && reach::onscreen(n))
            .map(|n| n.id)
        else {
            return Ok(false);
        };
        client
            .agent(&AgentControlRequest::Act(AgentAction::ScrollIntoView {
                node_id: id,
            }))
            .await?;
        tokio::time::sleep(Duration::from_millis(400)).await;

        let (tree, _) = inspect(client).await?;
        if !tree.nodes.iter().any(|n| n.id == id && reach::onscreen(n)) {
            return Ok(false);
        }
        client
            .agent(&AgentControlRequest::Act(AgentAction::DoubleClick {
                node_id: id,
            }))
            .await?;
    } else if click_named_quiet(client, &opener).await.is_err() {
        return Ok(false);
    }
    if settle_on(client, surface).await? {
        return Ok(true);
    }
    // One retry through the configured root, which can recover when
    // a modal on the current one is swallowing the direct hop.
    // Not for the project surface: its opener is a gesture, and repeating it as
    // a single click would fold the row rather than open it.
    if surface.opener == reach::DYNAMIC_DOCUMENT {
        return Ok(false);
    }
    if let Some(home) = reach::profile().home_opener.as_deref() {
        let _ = click_named_quiet(client, home).await;
    }
    tokio::time::sleep(Duration::from_millis(400)).await;
    if click_named_quiet(client, &opener).await.is_err() {
        return Ok(false);
    }
    settle_on(client, surface).await
}

/// Sweep the controls inside an open modal, then prove it can be dismissed.
///
/// Returns `true` when the dialog would not close by any means a person has:
/// its own dismiss controls, then Escape. That is a trap rather than a failed
/// button - every control behind it is unreachable while it is up - so the
/// caller stops rather than reporting hundreds of downstream failures that are
/// really one bug.
///
/// The dialog's own controls are pressed *before* the dismiss is tried, because
/// a `Cancel` that works would take them all off screen and they would never be
/// tested. Destructive-sounding ones are pressed last for the same reason.
async fn sweep_modal(
    client: &mut Client,
    opener: &str,
    here: &mut reach::Coverage,
    failures: &mut Vec<(String, String, String)>,
    surface: &str,
) -> Result<bool> {
    let (tree, _) = inspect(client).await?;
    let dismiss_ids: Vec<u64> = reach::dismissers(&tree.nodes)
        .into_iter()
        .map(|(id, _)| id)
        .collect();

    /*
     * The dialog's own controls, not the window's.
     *
     * A modal leaves the surface behind it in the tree, still `visible` and
     * still sized, exactly as a retained pane does. Sweeping everything on
     * screen can press retained-surface controls that are not in the dialog,
     * including a native-panel opener. The subtree that holds the dismiss
     * control is the dialog.
     */
    let scope = dismiss_ids
        .first()
        .map(|id| reach::enclosing_dialog(&tree.nodes, *id))
        .unwrap_or_default();
    if cli::trace() {
        for node in tree.nodes.iter().filter(|node| {
            scope.contains(&node.id)
                && (reach::profile()
                    .deferred_controls
                    .iter()
                    .any(|name| node.name.eq_ignore_ascii_case(name))
                    || reach::requires_isolated_outcome(&node.name))
        }) {
            println!(
                "      [modal] deferred to an outcome check: {:?}",
                node.name
            );
        }
    }
    let inner: Vec<(u64, String)> = tree
        .nodes
        .iter()
        .filter(|n| n.role == "button" && reach::onscreen(n) && n.enabled)
        .filter(|n| !n.name.trim().is_empty())
        .filter(|n| !dismiss_ids.contains(&n.id))
        .filter(|n| scope.contains(&n.id))
        .filter(|n| !reach::requires_manual_release_check(&n.name))
        .filter(|n| !reach::requires_isolated_outcome(&n.name))
        .filter(|n| {
            !reach::profile()
                .deferred_controls
                .iter()
                .any(|name| n.name.eq_ignore_ascii_case(name))
        })
        .map(|n| (n.id, n.name.clone()))
        .collect();

    for (id, name) in inner {
        let (before, _) = inspect(client).await?;
        if !before
            .nodes
            .iter()
            .any(|n| n.id == id && reach::onscreen(n))
        {
            continue;
        }
        if click_by_id(client, id).await.is_err() {
            continue;
        }
        // `revealed`, not `swept`: this control did not exist when the surface's
        // buttons were counted, so charging it to `swept` pushes the buckets
        // past the total and turns the consistency check negative.
        here.revealed += 1;
        if cli::trace() {
            println!("      [modal] clicked: {name:?}");
        }
        tokio::time::sleep(Duration::from_millis(180)).await;
        let (after, _) = inspect(client).await?;
        let case = sweep::Case {
            id,
            name: name.clone(),
            family: audit::family_of(&name),
            expect: sweep::expectation_for(&name, reach::is_inert_control(&name)),
        };
        if let Some(why) = sweep::judge(&case, &before.nodes, &after.nodes) {
            failures.push((
                surface.to_owned(),
                format!("{name} (in {opener} dialog)"),
                why,
            ));
        }
        // A control inside the dialog may itself have closed it.
        let (now, _) = inspect(client).await?;
        if !reach::modal_open(&now.nodes) {
            return Ok(false);
        }
    }

    // Every way out, in the order a person would reach for them.
    let (tree, _) = inspect(client).await?;
    for (id, name) in reach::dismissers(&tree.nodes) {
        if click_by_id(client, id).await.is_err() {
            continue;
        }
        // A dismisser belongs to the dialog, not to the surface underneath it.
        here.revealed += 1;
        tokio::time::sleep(Duration::from_millis(250)).await;
        let (after, _) = inspect(client).await?;
        if !reach::modal_open(&after.nodes) {
            return Ok(false);
        }
        failures.push((
            surface.to_owned(),
            format!("{name} (in {opener} dialog)"),
            "the dialog is still open; it did not dismiss".to_owned(),
        ));
    }

    // Escape, which `AppModal` binds and which is the last thing a person has.
    let _ = press_key(client, "escape", 1, "").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (after, _) = inspect(client).await?;
    if !reach::modal_open(&after.nodes) {
        return Ok(false);
    }
    failures.push((
        surface.to_owned(),
        format!("{opener} dialog"),
        "TRAPPED: no dismiss control and no Escape closes it".to_owned(),
    ));
    Ok(true)
}

/// Wait until the surface is actually showing, up to a few seconds.
///
/// Polled rather than slept: Analytics takes longer to build than the others
/// and a fixed 500ms wait declared it unopened while it was still on its way,
/// which sent the whole sweep down the retry path and then swept the wrong
/// surface twice. Polling costs nothing when the pane is already up.
async fn settle_on(client: &mut Client, surface: &reach::Surface) -> Result<bool> {
    for _ in 0..12 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        let (tree, _) = inspect(client).await?;
        if reach::on_surface(&tree.nodes, surface) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Sweep every surface, and account for every button in the tree.
///
/// The difference from `sweep`: it visits more than the screen the app opened
/// on, it opens what is closed and hovers what reveals on hover before planning,
/// and it reports what it could not reach instead of dropping it. A run that
/// covers a fifth of the window now says so.
///
/// `SWEEP_TRACE=1` names every control as it is pressed and every one that goes
/// missing, with the surface state at that moment. Every coverage bug found so
/// far looked identical from the summary - a large `vanished` count - and was
/// only separable from this: a stale id after a re-sort, a tab click walking off
/// the surface, and a collapse hiding its own neighbours all read the same until
/// you can see which click preceded them.
async fn run_cover(client: &mut Client, only: Option<&str>) -> Result<usize> {
    let mut total = reach::Coverage::default();
    let mut failures: Vec<(String, String, String)> = Vec::new();
    // Named, so the manual worklist at the end is what this run actually met.
    let mut skipped_manual: Vec<String> = Vec::new();
    // Named separately: these remain automated work, but need a disposable app.
    let mut skipped_isolated: Vec<String> = Vec::new();

    for surface in reach::surfaces() {
        if only.is_some_and(|want| want != surface.name) {
            continue;
        }
        if !open_surface(client, surface).await? {
            println!("- {:<10} could not be opened, skipping\n", surface.name);
            continue;
        }

        /*
         * Expanded within this surface only, and the surface re-checked after.
         *
         * `expand_everything` pressed every `Expand *` in the window, including
         * ones belonging to retained panes behind this one. Twelve such presses
         * can run before the current surface's plan is built, navigate away,
         * and leave that surface reporting zero buttons in a full run.
         */
        let opened = expand_everything(client, surface).await?;
        if !open_surface(client, surface).await? {
            println!("- {:<10} left during expansion, skipping\n", surface.name);
            continue;
        }
        materialize_deferred_content(client, surface).await?;
        let hovered = hover_all_rows(client).await?;

        /*
         * This surface's buttons, not the whole window's.
         *
         * A retained pane keeps its entire subtree alive and merely hidden, so
         * standing on one surface the tree can still hold every button from a
         * retained peer. Counting those here charges each surface for controls it was
         * not looking at: four surfaces reported 1106 buttons between them for
         * a window that has nowhere near that many, with 900 of them "hidden"
         * and the real coverage number buried.
         *
         * The subject is what is on screen now. A control retained from another
         * surface is that surface's to sweep, and it is swept when we stand
         * there.
         */
        let (tree, _) = inspect(client).await?;
        /*
         * Scoped by ancestry to the pane in front.
         *
         * A retained surface can keep real, `visible`, correctly-sized boxes behind an
         * open document pane, in the same horizontal band, so
         * neither position nor visibility separates them. Sweeping the union
         * means another surface's row controls are pressed under the current
         * surface's name while its own controls are crowded out of the plan.
         */
        let mine: std::collections::HashSet<u64> = reach::on_surface_subtree(&tree.nodes, surface)
            .into_iter()
            .collect();
        let buttons: Vec<&blitz_control_protocol::SemanticNode> = tree
            .nodes
            .iter()
            .filter(|n| n.role == "button" && !n.name.trim().is_empty())
            .filter(|n| n.visible)
            .filter(|n| mine.contains(&n.id))
            .collect();

        // Retained from other surfaces, reported so the difference between the
        // window's button count and this surface's is never silent.
        let retained = tree
            .nodes
            .iter()
            .filter(|n| n.role == "button" && !n.name.trim().is_empty() && !n.visible)
            .count();
        let mut here = reach::Coverage {
            in_tree: buttons.len(),
            hidden: 0,
            ..Default::default()
        };
        /*
         * Counted in a plain loop, not inside a `filter` closure.
         *
         * Written as a lazy filter that incremented the tallies, the counts were
         * still zero when the surface line printed them and only filled in as
         * the plan was consumed: the first run reported "0 swept, 0 unreachable,
         * UNACCOUNTED 106" for a surface it went on to sweep. A report that
         * undercounts itself is the exact failure this mode exists to remove.
         */
        let mut plan: Vec<(u64, String)> = Vec::new();
        let mut collapsers: Vec<(u64, String)> = Vec::new();
        let mut closers: Vec<(u64, String)> = Vec::new();
        for node in &buttons {
            if !reach::onscreen(node) {
                here.unreachable += 1;
            } else if reach::requires_isolated_outcome(&node.name) {
                here.isolated += 1;
                skipped_isolated.push(node.name.clone());
            } else if reach::profile()
                .fold_prefixes
                .iter()
                .any(|p| node.name.to_lowercase().starts_with(&p.to_lowercase()))
                || reach::profile()
                    .deferred_controls
                    .iter()
                    .any(|c| node.name.eq_ignore_ascii_case(c))
                || reach::folds_a_section(&node.name)
            {
                /*
                 * Swept, but last.
                 *
                 * A collapse closes the section its neighbours live in, and
                 * every control underneath goes to `visible=false` at the
                 * section's own origin. Pressing `Collapse Recent` first took
                 * most controls on the surface off screen and the run charged
                 * them all as vanished, which read as the app losing its
                 * buttons rather than the sweep hiding them.
                 *
                 * A configured document-creation control belongs here for the
                 * opposite reason: it opens what it creates, so pressing it
                 * early walks the sweep off its surface. It is still pressed after
                 * the rest of the surface is done.
                 *
                 * A configured fold control can hide an entire side panel and
                 * every row action beneath it. Deferring fold controls keeps
                 * those descendants in the sweep plan.
                 */
                collapsers.push((node.id, node.name.clone()));
            } else if reach::navigates(&node.name)
                || reach::opens_document_row(&tree.nodes, node.id)
            {
                // Swept as the opener of its own surface, not here: pressing it
                // mid-plan navigates away and every later button on this
                // surface reads as vanished.
                here.navigation += 1;
            } else if reach::requires_manual_release_check(&node.name) {
                // Never pressed unattended: a native chooser takes the user's
                // screen and cannot be dismissed from here.
                here.manual += 1;
                skipped_manual.push(node.name.clone());
            } else if reach::closes_a_surface(&node.name) {
                /*
                 * Swept, but after everything that stands on the tab it
                 * removes.
                 *
                 * Closing a tab can fall back to the root and retire a pane
                 * later surfaces are reached through. It is deferred rather
                 * than skipped, so the control is still pressed.
                 */
                closers.push((node.id, node.name.clone()));
            } else {
                plan.push((node.id, node.name.clone()));
            }
        }
        /*
         * Order within the plan: harmless first, then destructive, then the
         * disclosures that would hide either.
         *
         * A `Delete` removes its own row and shifts every row under it, so
         * running deletes early costs the sweep the controls it had not reached
         * yet. Sorting is stable, so controls otherwise keep tree order.
         */
        plan.sort_by_key(|(_, name)| {
            let lower = name.to_lowercase();
            u8::from(
                lower.starts_with("delete ")
                    || lower.starts_with("close ")
                    || lower.starts_with("remove ")
                    || lower.starts_with("retire "),
            )
        });
        plan.extend(collapsers);
        // Last of all: these retire the pane everything above stands on.
        plan.extend(closers);

        /*
         * Swept by name, and the surface is restored only when it has actually
         * been left.
         *
         * Re-navigating before every click looked safer and was much worse: the
         * re-render invalidated the very plan it was protecting, and the run
         * reported 98 of 125 controls vanished. Checking first costs one tree
         * read and leaves a working surface alone.
         */
        let mut done: HashMap<String, usize> = HashMap::new();
        let planned_total = plan.len();
        for (index, (planned_id, name)) in plan.into_iter().enumerate() {
            // What would be left if this control traps the window.
            let remaining = planned_total.saturating_sub(index + 1);
            let (mut before, _) = inspect(client).await?;
            if !reach::on_surface(&before.nodes, surface) {
                if !open_surface(client, surface).await? {
                    here.vanished += 1;
                    if cli::trace() {
                        println!("    left surface, could not return: {name:?}");
                    }
                    continue;
                }
                let (fresh, _) = inspect(client).await?;
                before = fresh;
            }

            /*
             * By id while it is still on screen, otherwise by name.
             *
             * Ids do not survive a re-render, and re-renders are ordinary here:
             * A sort control can reorder the list and every row can return with
             * a fresh id, the old ones retained as hidden 0x0 nodes at the
             * container's origin. Trusting the planned id after that pressed
             * nothing and charge the remaining controls as "vanished"
             * while all of them were on screen the whole time.
             *
             * Names are not unique - 161 on-screen buttons share 81 names, with
             * one label alone appearing thirty times - so the name path
             * takes the nth still-unpressed match rather than the first, which
             * is what stops one row absorbing every click aimed at its
             * neighbours.
             */
            let seen = done.entry(name.clone()).or_insert(0);
            let found = before
                .nodes
                .iter()
                .find(|n| n.id == planned_id && n.name == name && reach::onscreen(n))
                .or_else(|| {
                    before
                        .nodes
                        .iter()
                        .filter(|n| n.role == "button" && n.name == name && reach::onscreen(n))
                        .nth(*seen)
                });
            let Some(node) = found else {
                here.vanished += 1;
                if cli::trace() {
                    println!("    vanished: {name:?} (id {planned_id})");
                }
                continue;
            };
            *seen += 1;
            let id = node.id;
            if !reach::onscreen(node) || !node.enabled {
                here.vanished += 1;
                if cli::trace() {
                    let visible_buttons = before
                        .nodes
                        .iter()
                        .filter(|n| n.role == "button" && reach::onscreen(n))
                        .count();
                    println!(
                        "    offscreen/disabled: {name:?} visible={} bounds={:?} \
                         [on_surface={}, {visible_buttons} buttons on screen]",
                        node.visible,
                        node.bounds,
                        reach::on_surface(&before.nodes, surface),
                    );
                }
                continue;
            }
            let case = sweep::Case {
                id,
                name: name.clone(),
                family: audit::family_of(&name),
                expect: sweep::expectation_for(&name, reach::is_inert_control(&name)),
            };
            /*
             * Stop on a failed dispatch.
             *
             * This id came from the snapshot immediately above and names an
             * enabled, on-screen control. If the inspector cannot activate it,
             * continuing is not broader coverage: a native modal or halted UI
             * loop can leave the socket open without answering, making every
             * later control pay the full 60-second transport timeout. One run
             * then sat here for eighteen minutes after the ordinary sweep had
             * previously completed in two and a half. Preserve the exact
             * control in the error and stop before a transport failure is
             * multiplied by the rest of the plan.
             */
            if let Err(error) = click_by_id(client, id).await {
                bail!(
                    "could not activate {name:?} (id {id}) on {:?}; stopping the sweep: {error}",
                    surface.name
                );
            }
            here.swept += 1;
            if cli::trace() {
                println!("    clicked: {name:?}");
            }
            tokio::time::sleep(Duration::from_millis(200)).await;

            /*
             * If that opened a modal, sweep it and then prove it closes.
             *
             * Judging each button against its own name cannot prove the
             * foreground dialog can be dismissed. The relevant outcome is
             * whether the application can return to the covered surface.
             *
             * A dialog that will not dismiss is reported and the run continues
             * from a restart rather than grinding on against a window nobody
             * can use.
             */
            let (after_click, _) = inspect(client).await?;
            let after = if reach::modal_open(&after_click.nodes) {
                let trapped =
                    sweep_modal(client, &name, &mut here, &mut failures, &surface.name).await?;
                if trapped {
                    /*
                     * Everything still in the plan is unreachable behind the
                     * trap, and saying so is the point: a bucket that silently
                     * loses 64 controls is how this audit missed the fork
                     * dialog in the first place.
                     */
                    println!(
                        "  ! {:?} opened a dialog that will not dismiss - \
                         the rest of this surface is unreachable behind it",
                        name
                    );
                    here.blocked += remaining;
                    break;
                }
                inspect(client).await?.0
            } else {
                after_click
            };
            if sweep::judge(&case, &before.nodes, &after.nodes).is_some() {
                // Backend-backed controls can acknowledge immediately and
                // update the semantic tree on the next task. Retry only a
                // would-be failure, so fast controls do not all pay for the
                // slowest one.
                tokio::time::sleep(Duration::from_millis(800)).await;
                let (settled, _) = inspect(client).await?;
                if let Some(why) = sweep::judge(&case, &before.nodes, &settled.nodes) {
                    failures.push((surface.name.to_owned(), name, why));
                }
            }
        }

        // Printed after the sweep, because `swept` is not known until then and
        // a coverage line that reports zero for work it is about to do is worse
        // than no line at all.
        println!(
            "= {:<10} {} ({} sections opened, {} rows hovered, {} retained elsewhere)",
            surface.name,
            here.line(),
            opened,
            hovered,
            retained
        );

        total.in_tree += here.in_tree;
        total.swept += here.swept;
        total.unreachable += here.unreachable;
        total.hidden += here.hidden;
        total.vanished += here.vanished;
        total.navigation += here.navigation;
        total.manual += here.manual;
        total.isolated += here.isolated;
        // `blocked` and `revealed` were missing here, so a control trapped
        // behind an undismissable dialog counted on its surface line and then
        // disappeared from the run total - the one line most likely to be
        // quoted as the coverage number.
        total.blocked += here.blocked;
        total.revealed += here.revealed;
    }

    println!("\n{}", total.line());
    /*
     * The exceptions, named, every run.
     *
     * A skipped control that is only a number in a bucket is a control nobody
     * remembers to test. Native dialogs that cannot be closed and controls
     * that open external destinations need a person, so the report says so
     * rather than implying the automated run covered them.
     */
    if total.manual > 0 {
        println!(
            "\n{} control(s) need the manual release pass:",
            total.manual
        );
        // Only the ones this run actually met, so the list is a worklist rather
        // than a catalogue of everything that could theoretically be skipped.
        let mut seen: Vec<&str> = skipped_manual.iter().map(String::as_str).collect();
        seen.sort_unstable();
        seen.dedup();
        for label in seen {
            let command = reach::profile()
                .manual_controls
                .iter()
                .find(|exception| label.starts_with(exception.label.as_str()))
                .map(|exception| exception.command.as_str())
                .unwrap_or("(unmapped manual control)");
            println!("  {label:<38} {command}");
        }
    }
    if total.isolated > 0 {
        println!(
            "\n{} control(s) require an isolated outcome check:",
            total.isolated
        );
        skipped_isolated.sort_unstable();
        skipped_isolated.dedup();
        for label in skipped_isolated {
            println!("  {label}");
        }
    }
    if failures.is_empty() {
        if total.isolated > 0 {
            println!("every broadly swept button acted; isolated controls remain listed above");
        } else {
            println!("every reached button acted");
        }
    } else {
        println!("\n{} did not act:\n", failures.len());
        for (surface, name, why) in &failures {
            println!(
                "  [{surface}] {:<40} {why}",
                name.chars().take(40).collect::<String>()
            );
        }
    }
    Ok(failures.len())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = <cli::Cli as clap::Parser>::parse();
    cli::set_trace(cli.trace);
    cli::set_pace(cli.pace);
    cli::set_app_profile(cli.app.clone());

    // The inventory reads the check list, not the application, so it answers
    // "what is covered" with nothing running. Before the descriptor lookup for
    // that reason: requiring a live instance to list the checks is what kept
    // the coverage question unanswerable.
    if let cli::Command::List { checks } = &cli.command {
        print!(
            "{}",
            qa::manifest(checks.as_deref()).map_err(|error| eyre!(error))?
        );
        return Ok(());
    }
    if let cli::Command::Reconcile { inventory, checks } = &cli.command {
        let failures = reconcile_inventory(inventory, checks.as_deref())?;
        if failures > 0 {
            std::process::exit(1);
        }
        return Ok(());
    }

    // Before attaching to anything: a run that drives an application against a
    // profile it does not have is worse than one that refuses to start, and
    // failing here names the missing file rather than reporting nothing found.
    app::AppProfile::load(cli.app.as_deref()).map_err(|error| eyre!(error))?;

    let descriptor = inspector::discover(cli.descriptor.as_deref().and_then(|p| p.to_str()))?;
    descriptor.warn_if_stale();

    let verbose = cli.command.is_dump();
    if verbose {
        println!("descriptor: {}", descriptor.path.display());
        println!("{}", report::dump(&descriptor.raw, usize::MAX));
    }

    let mut client = Client::connect(&descriptor.socket_path()).await?;
    let initialize = client.initialize().await?;
    if verbose {
        println!("\n== initialize ==");
        println!("{}", report::dump(&initialize, 800));
        println!("\n== tools ==");
        let tools = client.tools_list().await?;
        println!("{}", report::dump(&tools, 1200));
    }

    match cli.command {
        cli::Command::Metrics => {
            println!("\n== metrics ==");
            let answer = client.diagnostics(&DiagnosticsRequest::Metrics).await?;
            println!("{}", report::dump(&answer.envelope, 2000));
        }
        cli::Command::Watch { seconds } => {
            println!("\n== observing metrics/console/runtimeErrors for {seconds}s ==");
            // Tolerates a protocol error on purpose: the server answers
            // `streamingUnavailable` because `observe` is not implemented, and
            // reporting that is more useful than exiting on it.
            let answer = client
                .diagnostics_envelope(&DiagnosticsRequest::Observe {
                    streams: vec![
                        DebugStream::Metrics,
                        DebugStream::Console,
                        DebugStream::RuntimeErrors,
                    ],
                })
                .await?;
            println!("{}", report::dump(&answer, 400));
            for message in client.drain(seconds).await? {
                println!("{}", report::dump(&message, 400));
            }
        }
        cli::Command::Frames => report::show_frames(&metrics(&mut client).await?),
        cli::Command::Tree => {
            println!("\n== semantic tree ==");
            // The Python sent `maxDepth` here, which the server rejects: fields
            // inside protocol variants are snake_case. Encoding from the shared
            // type is what makes that unrepresentable rather than a silent
            // error nobody read.
            let answer = client
                .agent(&AgentControlRequest::Inspect {
                    root: None,
                    max_depth: 3,
                })
                .await?;
            println!("{}", report::dump(&answer.envelope, 3000));
        }
        cli::Command::Find {
            pattern,
            role,
            visible,
            hidden,
            painted,
            offscreen,
            disabled,
            count,
            limit,
        } => {
            find(
                &mut client,
                &pattern,
                &role,
                visible,
                hidden,
                painted,
                offscreen,
                disabled,
                count,
                limit,
            )
            .await?;
        }
        cli::Command::Layout { name } => {
            layout(&mut client, &name).await?;
        }
        cli::Command::Transcript => transcript(&mut client).await?,
        cli::Command::Paint { name, min_area } => {
            paint(&mut client, &name, min_area).await?;
        }
        cli::Command::Nodes => {
            nodes(&mut client).await?;
        }
        cli::Command::Panes => panes(&mut client).await?,
        cli::Command::Dom { name, depth } => {
            dom(&mut client, &name, depth).await?;
        }
        cli::Command::Spill { axis, tolerance } => {
            spill(&mut client, &axis, tolerance).await?;
        }
        cli::Command::Idle => report::show("idle", &metrics(&mut client).await?),
        // Every counter the app publishes is cumulative since launch, and a
        // mount costs thousands of DOM writes. Reading them once and calling
        // the total a rate is how "3,698 attribute writes" got recorded as 230
        // a second when it was actually the startup mount, counted once.
        // Two reads and a subtraction is the only honest way to ask what an
        // idle app is still doing.
        cli::Command::Drift { seconds } => {
            let before = metrics(&mut client).await?;
            println!("== holding still for {seconds}s, nothing driven ==");
            tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
            let after = metrics(&mut client).await?;
            let frames_of =
                |m: &RendererMetrics| m.frame_window.as_ref().map(|w| w.frames_total).unwrap_or(0);
            let frames = frames_of(&after).saturating_sub(frames_of(&before));
            println!(
                "frames={frames} over {seconds}s = {:.1}fps with no input",
                frames as f64 / seconds
            );
            report::show_delta(&before, &after, 0);
        }
        /*
         * The owner's blinking-rectangle repro, asserted rather than described.
         *
         * The repro is a project with 0 items and the item list expanded, which
         * is the state this reads. It exists because the fault was reported
         * four times and twice called fixed from a reading that did not
         * actually cover it: an idle window is not quiet here, and saying so
         * once in prose has not been enough.
         *
         * What it asserts, and why each is the honest form of the question:
         *
         * - `missed_refreshes` over the sample window. A blink is a frame that
         *   did not land, so this is the number that has to be zero. It is a
         *   count over a fixed 256-frame window, not a rate, so it is
         *   comparable between runs.
         * - the worst frame interval against the display's own period. A single
         *   72ms gap on a 60Hz display is four dropped refreshes and is visible
         *   as a flash; a mean of 13ms is not. The mean is what a naive reading
         *   reports and it hides exactly this.
         *
         * Both are read from one metrics response so they describe the same
         * window. Exits non-zero when the fault is present, so it can gate a
         * fix instead of being read by eye.
         *
         * Deliberately not asserted: fps. It averages over the window and a
         * blink does not move it enough to fail on, which is how "the window is
         * quiet" was concluded from a sample that contained a 477ms stall.
         */
        /*
         * Controls that are meant to be hidden and still take up space.
         *
         * The class of fault this catches has now shipped twice, and neither
         * time did a component test see it, because both are about geometry in
         * the real renderer rather than behaviour in a DOM stub. A field hidden
         * by styling the input alone leaves the library's wrapper in the
         * layout: measured beside every project name, a 101x46 box painting as
         * a black rectangle and squeezing the name next to it down to a few
         * characters.
         *
         * The rule is narrow on purpose. A node whose accessible name says it
         * belongs to an inactive control - a rename editor with no editor open
         * - must not own a painted box. Anything genuinely displayed is
         * expected to have one, so this reports only boxes that are both
         * sizeable and attached to something the tree calls hidden.
         */
        cli::Command::Ghost { min_area, max } => {
            // Second argument so `ghost 64 0` can still demand a perfectly clean
            // tree when a caller wants that, without editing this default.
            let answer = client
                .diagnostics(&DiagnosticsRequest::Snapshot(SnapshotRequest {
                    include_dom: true,
                    include_layout: true,
                    include_computed_style: false,
                }))
                .await?;
            let DebugResponse::Snapshot(snapshot) = answer.response else {
                bail!("asked for a layout snapshot, got {:?}", answer.response);
            };

            let mut boxes: HashMap<u64, (f64, f64, f64, f64)> = HashMap::new();
            if let Some(rows) = snapshot.layout.as_ref().and_then(|v| v.as_array()) {
                for row in rows {
                    let Some(id) = row.get("nodeId").and_then(|v| v.as_u64()) else {
                        continue;
                    };
                    let read = |key: &str, index: usize| {
                        row.get("bounds")
                            .and_then(|b| b.get(key).or_else(|| b.get(index)))
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0)
                    };
                    boxes.insert(
                        id,
                        (
                            read("x", 0),
                            read("y", 1),
                            read("width", 2),
                            read("height", 3),
                        ),
                    );
                }
            }

            let nodes = snapshot
                .dom
                .as_ref()
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();

            let mut ghosts = Vec::new();
            for node in &nodes {
                // The snapshot spells this as `visible`, which is what `dom`
                // mode prints as HIDDEN. Reading a `hidden` key instead found
                // nothing and reported a clean run, which is the failure mode a
                // check like this must not have.
                let visible = node
                    .get("visible")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if visible {
                    continue;
                }
                let Some(id) = node.get("id").and_then(|v| v.as_u64()) else {
                    continue;
                };
                let Some(&(x, y, w, h)) = boxes.get(&id) else {
                    continue;
                };
                if w * h >= min_area {
                    let name = node
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    ghosts.push((id, name, x, y, w, h));
                }
            }
            ghosts.sort_by(|a, b| (b.4 * b.5).total_cmp(&(a.4 * a.5)));

            println!(
                "{} nodes inspected, reporting hidden boxes of {min_area}px2 or more",
                nodes.len()
            );
            for (id, name, x, y, w, h) in &ghosts {
                println!("  {id:>10}  {w:>7.1}x{h:<7.1} at {x:.0},{y:.0}  {name}");
            }
            if ghosts.is_empty() {
                println!("\nno ghosts: nothing hidden is holding a painted box");
            } else {
                println!(
                    "\nGHOSTS PRESENT: {} hidden node(s) still occupy layout",
                    ghosts.len()
                );
                // Retention is deliberate, so some ghosts are the design rather
                // than a leak: an application may deliberately retain a bounded
                // number of mounted-but-hidden panes. Failing on any ghost at
                // all therefore rejects valid retention policies.
                //
                // What a leak looks like: the 2026-08-20 window that painted one
                // flat colour and took no clicks measured 2,404 ghosts on a
                // comparable tree (5,527 nodes vs 5,098), a 41x rise. The budget
                // sits between the two, far enough above the healthy figure that
                // ordinary drift in the retained panes does not trip it.
                if ghosts.len() > max {
                    println!(
                        "over budget: {} ghosts, limit {max}. Hidden subtrees are not \
                         being unmounted.",
                        ghosts.len()
                    );
                    std::process::exit(1);
                }
                println!("within budget: limit {max}");
            }
        }
        cli::Command::Blink { allowed_missed } => {
            let reading = metrics(&mut client).await?;
            report::show("blink", &reading);

            let Some(window) = reading.frame_window.as_ref() else {
                eyre::bail!(
                    "the app published no frame window, so there is nothing to assert; \
                     launch it with --blitz-deep-profiling"
                );
            };

            // The refresh period the app itself reports, so this stays right on
            // a 120Hz panel rather than assuming 60. Unknown falls back to 60,
            // which is the more forgiving of the two.
            let period_ms = 1000.0
                / window
                    .display_refresh_hz
                    .filter(|hz| *hz > 0.0)
                    .unwrap_or(60.0);
            // Two periods: one late frame is a hiccup, two is a gap a person
            // sees. Anything under this is not the reported fault.
            let interval_budget = period_ms * 2.0;
            let worst_interval = window.interval.max_ms;

            println!();
            println!("== the reported repro: a project with 0 items, item list expanded ==");
            println!(
                "  missed refreshes : {} over {} frames (allowed {allowed_missed})",
                window.missed_refreshes, window.window_frames
            );
            println!(
                "  worst interval   : {worst_interval:.1}ms against a {period_ms:.1}ms refresh \
                 (budget {interval_budget:.1}ms)"
            );

            let mut faults = Vec::new();
            if window.missed_refreshes > allowed_missed {
                faults.push(format!(
                    "{} missed refreshes over {} frames",
                    window.missed_refreshes, window.window_frames
                ));
            }
            if worst_interval > interval_budget {
                faults.push(format!(
                    "a {worst_interval:.1}ms frame interval, {:.1}x the refresh period",
                    worst_interval / period_ms
                ));
            }

            if faults.is_empty() {
                println!("\nno blink: the window is quiet by both measures");
            } else {
                println!("\nBLINK PRESENT: {}", faults.join(", "));
                std::process::exit(1);
            }
        }
        cli::Command::Click { name, id } => {
            nodes(&mut client).await?;
            match (name, id) {
                (Some(name), None) => click_named(&mut client, &name).await?,
                (None, Some(node_id)) => {
                    click_by_id(&mut client, node_id).await?;
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    println!("activated node {node_id}");
                }
                _ => unreachable!("clap requires exactly one click selector"),
            }
        }
        cli::Command::Capture { name, scale } => {
            capture(&mut client, &name, scale as f32).await?;
        }
        cli::Command::Audit { family } => {
            let faults = run_audit(&mut client, family.as_deref()).await?;
            if faults > 0 {
                std::process::exit(1);
            }
        }
        cli::Command::Sweep { family } => {
            let failures = run_sweep(&mut client, family.as_deref()).await?;
            if failures > 0 {
                std::process::exit(1);
            }
        }
        /*
         * A real pointer press, as opposed to a synthesised click.
         *
         * `click` dispatches an `AgentAction::Click` at a node id, which is not
         * the path a person's mouse takes. The owner reports controls working
         * that the sweep calls dead, so the two have to be separable: if a
         * control acts under `press` and not under `click`, the finding is the
         * harness's, and every result that rests on `click` needs re-reading.
         */
        cli::Command::Press { name } => {
            let (snapshot, _) = inspect(&mut client).await?;
            let wanted = name.to_lowercase();
            let Some(node) = snapshot
                .nodes
                .iter()
                .filter(|n| n.name.to_lowercase().contains(&wanted))
                .filter(|n| n.visible && n.enabled)
                .find(|n| n.bounds.is_some_and(|b| b[2] > 0.0 && b[3] > 0.0))
            else {
                bail!("no visible, enabled, sized node matching {name:?}");
            };
            let b = node.bounds.unwrap();
            let (x, y) = (b[0] + b[2] / 2.0, b[1] + b[3] / 2.0);
            println!("pressing {:?} at {x:.0},{y:.0}", node.name);
            // Move first: a press at a point the document never saw hovered is
            // not what a mouse does, and hover state gates some controls.
            for phase in [PointerPhase::Move, PointerPhase::Down, PointerPhase::Up] {
                client
                    .agent(&AgentControlRequest::Act(AgentAction::Input(
                        InputCommand::Pointer {
                            phase,
                            x,
                            y,
                            button: 0,
                            modifiers: Modifiers::default(),
                        },
                    )))
                    .await?;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
            println!("pressed");
        }
        cli::Command::Cover { surface } => {
            let failures = run_cover(&mut client, surface.as_deref()).await?;
            if failures > 0 {
                std::process::exit(1);
            }
        }
        cli::Command::Inventory {
            surface,
            require_outcomes,
        } => {
            let failures = run_inventory(&mut client, surface.as_deref(), require_outcomes).await?;
            if failures > 0 {
                std::process::exit(1);
            }
        }
        cli::Command::Qa { selector, checks } => {
            let failed = run_qa(&mut client, selector.as_deref(), checks.as_deref()).await?;
            if failed > 0 {
                std::process::exit(1);
            }
        }
        cli::Command::Reveal { name } => {
            let (snapshot, _) = inspect(&mut client).await?;
            let Some(target) = snapshot
                .nodes
                .iter()
                .find(|node| node.name.contains(&name) && node.bounds.is_some())
            else {
                bail!("no node named {name:?}");
            };
            let before = target.bounds.unwrap();
            let id = target.id;
            println!("{id} {:?} y={:.1}", target.role, before[1]);
            client
                .agent(&AgentControlRequest::Act(AgentAction::ScrollIntoView {
                    node_id: id,
                }))
                .await?;
            tokio::time::sleep(Duration::from_millis(400)).await;
            let (after_snapshot, _) = inspect(&mut client).await?;
            let after = after_snapshot
                .nodes
                .iter()
                .find(|node| node.id == id)
                .and_then(|node| node.bounds);
            match after {
                Some(b) => println!("after: y={:.1} (moved {:.1})", b[1], b[1] - before[1]),
                None => println!("after: the node is gone from the tree"),
            }
        }
        cli::Command::Key { name, count, over } => {
            // An empty `over` falls back to whatever the profile calls the
            // main scrolling region, so the common case needs no argument.
            let fallback = reach::profile()
                .transcript_region
                .clone()
                .unwrap_or_default();
            let over = if over.is_empty() { &fallback } else { &over };
            press_key(&mut client, &name, count as usize, over).await?;
        }
        cli::Command::Type { count, name } => {
            nodes(&mut client).await?;
            type_keys(&mut client, count as usize, &name).await?;
        }
        // Wheel events go to whatever the document last saw hovered, which an
        // injected pointer move does not reliably set, so they scroll nothing.
        // This asks the node's own scroll container to move, which is what
        // `scroll` should have been able to do all along.
        cli::Command::Drag { name, dy, steps } => {
            let fallback = reach::profile()
                .transcript_region
                .clone()
                .unwrap_or_default();
            let name = if name.is_empty() { &fallback } else { &name };
            let times = steps as usize;
            let (snapshot, _) = inspect(&mut client).await?;
            let Some(target) = snapshot
                .nodes
                .iter()
                .filter(|node| node.visible && node.name.contains(name))
                .filter_map(|node| node.bounds.map(|b| (node, b)))
                .max_by(|a, b| {
                    (a.1[2] * a.1[3])
                        .partial_cmp(&(b.1[2] * b.1[3]))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(node, _)| node.id)
            else {
                bail!("no visible node named {name:?}");
            };
            println!("scrolling node {target} by {dy} x{times}");
            for _ in 0..times {
                client
                    .agent(&AgentControlRequest::Act(AgentAction::ScrollBy {
                        node_id: target,
                        delta_x: 0.0,
                        delta_y: dy,
                    }))
                    .await?;
                sleep_pace().await;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        cli::Command::Scroll { ticks, delta, over } => {
            let fallback = reach::profile()
                .transcript_region
                .clone()
                .unwrap_or_default();
            let over = if over.is_empty() { &fallback } else { &over };
            let count = nodes(&mut client).await?;
            hover_over(&mut client, over).await?;
            report::show("before", &metrics(&mut client).await?);
            scroll(&mut client, ticks as usize, delta).await?;
            report::show("after", &metrics(&mut client).await?);
            println!("tree size during run: {count} nodes");
        }
        // No catch-all: the parser rejects an unknown command, with the list of
        // real ones, before any of this runs.
        cli::Command::List { .. } | cli::Command::Reconcile { .. } => {
            unreachable!("handled before the client connects")
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        InventoryClass, inventory_class, name_matches, outcome_check_ids, outcome_verdict,
        painted_named, saved_control_row, saved_controls, selector_matches_node,
    };
    use crate::qa::{Check, Expect};
    use blitz_control_protocol::SemanticNode;

    fn component(name: &str, enabled: bool, visible: bool) -> SemanticNode {
        SemanticNode {
            id: 1,
            parent: None,
            role: "button".into(),
            name: name.into(),
            value: None,
            enabled,
            visible,
            selected: false,
            bounds: Some([0.0, 0.0, 20.0, 20.0]),
        }
    }

    #[test]
    fn inventory_categories_are_mutually_exclusive() {
        assert_eq!(
            inventory_class(&component("Import data", true, true), true, false),
            InventoryClass::Manual
        );
        assert_eq!(
            inventory_class(&component("Restart application", true, true), false, true),
            InventoryClass::Isolated
        );
        assert_eq!(
            inventory_class(&component("", false, false), false, false),
            InventoryClass::Anonymous
        );
        assert_eq!(
            inventory_class(&component("Save", false, true), false, false),
            InventoryClass::Disabled
        );
        assert_eq!(
            inventory_class(&component("Hidden", true, false), false, false),
            InventoryClass::Unreachable
        );
        assert_eq!(
            inventory_class(&component("Synchronize", true, true), false, false),
            InventoryClass::Reachable
        );
    }

    fn check(id: &str, click: Option<&str>, subject: &str) -> Check {
        Check {
            id: id.into(),
            group: "coverage".into(),
            what: "a rendered outcome".into(),
            open: None,
            hover: None,
            click: click.map(str::to_owned),
            type_into: None,
            text: None,
            key: None,
            key_on: None,
            compare: None,
            covers: Vec::new(),
            press: false,
            subject: subject.into(),
            expect: Expect::Paints,
        }
    }

    #[test]
    fn role_qualified_coverage_does_not_credit_a_same_named_wrong_role() {
        let button = component("Rename project", true, true);
        assert!(!selector_matches_node(&button, "textbox:Rename project"));

        let mut textbox = button.clone();
        textbox.role = "textbox".into();
        assert!(selector_matches_node(&textbox, "textbox:Rename project"));
    }

    #[test]
    fn check_preconditions_require_a_painted_matching_role() {
        let button = component("Rename project", true, true);
        assert!(painted_named(std::slice::from_ref(&button), "Rename"));
        assert!(!painted_named(
            std::slice::from_ref(&button),
            "textbox:Rename project"
        ));

        let mut textbox = button;
        textbox.role = "textbox".into();
        assert!(painted_named(
            std::slice::from_ref(&textbox),
            "textbox:Rename project"
        ));
        textbox.bounds = Some([0.0, 0.0, 0.0, 0.0]);
        assert!(!painted_named(
            std::slice::from_ref(&textbox),
            "textbox:Rename project"
        ));
    }

    #[test]
    fn outcome_coverage_names_the_checks_that_drive_or_assert_a_control() {
        let checks = [
            check("rename", Some("Rename"), "textbox:Rename project"),
            check("save", Some("Save"), "Saved"),
        ];
        assert_eq!(
            outcome_check_ids(&component("Rename project", true, true), &checks),
            vec!["rename"]
        );
        assert!(outcome_check_ids(&component("Delete project", true, true), &checks).is_empty());
    }

    #[test]
    fn an_explicit_family_selector_credits_repeated_component_rows() {
        let mut check = check("offer-model", Some("Offer Default"), "Offer Default");
        check.covers.push("checkbox:Offer ".into());
        let mut sibling = component("Offer Sonnet", true, true);
        sibling.role = "checkbox".into();
        assert_eq!(outcome_check_ids(&sibling, &[check]), vec!["offer-model"]);
    }

    #[test]
    fn outcome_waiting_rejects_an_unchanged_refresh_indicator() {
        let mut check = check("refresh", Some("Refresh"), "Refresh generation");
        check.expect = Expect::NameChanges;
        let before = component("Refresh generation 1", true, true);
        let after = before.clone();
        assert!(outcome_verdict(&check, &[before], &[after], None, None).is_err());
    }

    #[test]
    fn outcome_waiting_accepts_the_completed_refresh_indicator() {
        let mut check = check("refresh", Some("Refresh"), "Refresh generation");
        check.expect = Expect::NameChanges;
        let before = component("Refresh generation 1", true, true);
        let after = component("Refresh generation 2", true, true);
        assert!(outcome_verdict(&check, &[before], &[after], None, None).is_ok());
    }

    #[test]
    fn saved_inventory_rows_keep_quoted_control_names_with_commas() {
        let row = saved_control_row(
            r#"  home,7,button,"Delete alpha, beta",reachable-unverified,no outcome check matched"#,
        )
        .expect("control row");
        assert_eq!(row.surface, "home");
        assert_eq!(row.role, "button");
        assert_eq!(row.name, "Delete alpha, beta");
        assert_eq!(row.classification, "reachable-unverified");
    }

    #[test]
    fn a_saved_report_must_have_an_inventory_table() {
        assert!(saved_controls("components: 3").is_err());
        assert_eq!(
            saved_controls(
                "controls[1]{surface,id,role,name,classification,reason}:\n  home,7,button,Save,reachable-unverified,none"
            )
            .expect("controls")
            .len(),
            1
        );
    }

    /// A bare word is a substring, because that is how a control is recalled.
    #[test]
    fn a_bare_pattern_is_a_substring() {
        assert!(name_matches("Rename project", "rename"));
        assert!(name_matches("Rename project", "project"));
        assert!(!name_matches("Rename project", "delete"));
    }

    #[test]
    fn a_trailing_star_anchors_the_front() {
        assert!(name_matches("chat with agent", "chat*"));
        assert!(!name_matches("open chat", "chat*"));
    }

    #[test]
    fn a_leading_star_anchors_the_end() {
        assert!(name_matches("open chat", "*chat"));
        assert!(!name_matches("chat with agent", "*chat"));
    }

    #[test]
    fn stars_at_both_ends_match_anywhere() {
        assert!(name_matches("the chat panel", "*chat*"));
        assert!(!name_matches("Rename project", "*chat*"));
    }

    /// Matching is case insensitive: a name is remembered by its words, not by
    /// the capitalisation a designer chose.
    #[test]
    fn matching_ignores_case() {
        assert!(name_matches("Rename Project", "rename project"));
        assert!(name_matches("CHAT", "chat*"));
    }

    #[test]
    fn a_lone_star_matches_everything() {
        assert!(name_matches("anything at all", "*"));
        assert!(name_matches("", "*"));
    }
}
