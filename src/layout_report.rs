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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayoutRow {
    node_id: u64,
    bounds: [f64; 4],
    scroll_offset: [f64; 2],
    client_size: [f64; 2],
    scroll_size: [f64; 2],
    scroll_range: [f64; 2],
    border: [f64; 4],
    padding: [f64; 4],
    content_size: [f64; 2],
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
        let [x, y, width, height] = row.bounds;
        let [scroll_x, scroll_y] = row.scroll_offset;
        let [range_width, range_height] = row.scroll_range;
        let [client_width, client_height] = row.client_size;
        let [content_width, content_height] = row.content_size;
        let [border_top, border_right, border_bottom, border_left] = row.border;
        let [padding_top, padding_right, padding_bottom, padding_left] = row.padding;
        let [scroll_width, scroll_height] = row.scroll_size;

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
            x,
            y,
            width,
            height,
            scroll_x,
            scroll_y,
            range_width,
            range_height,
            client_width,
            client_height,
            content_width,
            content_height,
            border_top,
            border_right,
            border_bottom,
            border_left,
            padding_top,
            padding_right,
            padding_bottom,
            padding_left,
            scroll_width,
            scroll_height,
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
        assert_eq!(rows[0].content_size, [10.0, 11.0]);
        assert_eq!(rows[0].border, [1.0, 2.0, 3.0, 4.0]);
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
