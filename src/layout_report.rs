//! Typed reporting for the renderer's layout diagnostic.
//!
//! The shared protocol still carries snapshots as JSON because DOM, layout,
//! and computed-style payloads are independently optional. That is a wire
//! boundary, not permission for the command to scatter field strings and
//! positional indices through its formatter. Decode once here and let schema
//! drift fail at the boundary with the field that disagreed.

use std::collections::HashMap;

use blitz_control_protocol::{DebugResponse, DiagnosticsRequest, SnapshotRequest};
use eyre::{Context, Result, bail};
use serde::Deserialize;

use crate::inspector::Client;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(transparent)]
struct Bounds([f64; 4]);

impl Bounds {
    fn x(self) -> f64 {
        self.0[0]
    }

    fn y(self) -> f64 {
        self.0[1]
    }

    fn width(self) -> f64 {
        self.0[2]
    }

    fn height(self) -> f64 {
        self.0[3]
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(transparent)]
struct Size([f64; 2]);

impl Size {
    fn width(self) -> f64 {
        self.0[0]
    }

    fn height(self) -> f64 {
        self.0[1]
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(transparent)]
struct Offset([f64; 2]);

impl Offset {
    fn x(self) -> f64 {
        self.0[0]
    }

    fn y(self) -> f64 {
        self.0[1]
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(transparent)]
struct Edges([f64; 4]);

impl Edges {
    fn top(self) -> f64 {
        self.0[0]
    }

    fn right(self) -> f64 {
        self.0[1]
    }

    fn bottom(self) -> f64 {
        self.0[2]
    }

    fn left(self) -> f64 {
        self.0[3]
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayoutRow {
    node_id: u64,
    bounds: Bounds,
    scroll_offset: Offset,
    client_size: Size,
    scroll_size: Size,
    scroll_range: Size,
    border: Edges,
    padding: Edges,
    content_size: Size,
}

#[derive(Debug, Deserialize)]
struct DomNode {
    id: u64,
    #[serde(default)]
    role: String,
    #[serde(default)]
    name: String,
}

fn decode_rows(value: Option<serde_json::Value>) -> Result<Vec<LayoutRow>> {
    serde_json::from_value(value.unwrap_or_else(|| serde_json::json!([])))
        .wrap_err("renderer returned an invalid layout diagnostic schema")
}

fn decode_nodes(value: Option<serde_json::Value>) -> Result<Vec<DomNode>> {
    serde_json::from_value(value.unwrap_or_else(|| serde_json::json!([])))
        .wrap_err("renderer returned an invalid DOM diagnostic schema")
}

/// Print the live box of every named node, optionally filtered by name.
///
/// A layout complaint that cannot be reproduced from markup is answered by
/// boxes the running app computed, not by another screenshot.
pub async fn layout(client: &mut Client, want: &str) -> Result<()> {
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

    let rows = decode_rows(snapshot.layout)?;
    let nodes = decode_nodes(snapshot.dom)?;
    let rows: HashMap<u64, LayoutRow> = rows.into_iter().map(|row| (row.node_id, row)).collect();

    let mut shown = 0usize;
    for node in &nodes {
        if !want.is_empty() && !node.name.contains(want) && !node.role.contains(want) {
            continue;
        }
        let Some(row) = rows.get(&node.id) else {
            continue;
        };
        let bounds = row.bounds;

        // Border/padding use CSS shorthand order: top, right, bottom, left.
        // Every value below is named and fixed-size; a missing or malformed
        // field fails during decoding instead of silently printing NaN.
        println!(
            "{:>6}  {:<16} {:>8.1} {:>8.1} {:>8.1} {:>8.1}  scroll={:.1},{:.1} range={:.1},{:.1} \
             border-box={:.1},{:.1} content-box={:.1},{:.1} \
             border={:.1},{:.1},{:.1},{:.1} padding={:.1},{:.1},{:.1},{:.1} \
             scrollable={:.1},{:.1}  {}",
            node.id,
            node.role,
            bounds.x(),
            bounds.y(),
            bounds.width(),
            bounds.height(),
            row.scroll_offset.x(),
            row.scroll_offset.y(),
            row.scroll_range.width(),
            row.scroll_range.height(),
            row.client_size.width(),
            row.client_size.height(),
            row.content_size.width(),
            row.content_size.height(),
            row.border.top(),
            row.border.right(),
            row.border.bottom(),
            row.border.left(),
            row.padding.top(),
            row.padding.right(),
            row.padding.bottom(),
            row.padding.left(),
            row.scroll_size.width(),
            row.scroll_size.height(),
            node.name.chars().take(60).collect::<String>()
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

#[cfg(test)]
mod tests {
    use super::{decode_nodes, decode_rows};

    #[test]
    fn layout_wire_shape_decodes_to_named_fields() {
        let rows = decode_rows(Some(serde_json::json!([{
            "nodeId": 7,
            "bounds": [1.0, 2.0, 30.0, 40.0],
            "scrollOffset": [3.0, 4.0],
            "clientSize": [30.0, 40.0],
            "scrollSize": [50.0, 60.0],
            "scrollRange": [20.0, 20.0],
            "border": [1.0, 2.0, 3.0, 4.0],
            "padding": [5.0, 6.0, 7.0, 8.0],
            "contentSize": [10.0, 11.0]
        }])))
        .unwrap();
        let nodes = decode_nodes(Some(serde_json::json!([{
            "id": 7,
            "role": "button",
            "name": "Save"
        }])))
        .unwrap();

        assert_eq!(rows[0].node_id, 7);
        assert_eq!(rows[0].content_size.width(), 10.0);
        assert_eq!(rows[0].border.left(), 4.0);
        assert_eq!(nodes[0].name, "Save");
    }

    #[test]
    fn malformed_layout_fails_at_the_schema_boundary() {
        let error = decode_rows(Some(serde_json::json!([{
            "nodeId": 7,
            "bounds": [1.0, 2.0]
        }])))
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("invalid layout diagnostic schema")
        );
    }
}
