//! Finding a running inspector, and talking to it.
//!
//! The transport is deliberately not hand-rolled. Frames are length-prefixed
//! rather than newline-delimited, which is why a naive socket read hangs, and
//! `endpoint_libs`' `framed_json` is the same codec the server writes with.

use std::path::{Path, PathBuf};
use std::time::Duration;

use blitz_control_protocol::{
    AgentControlRequest, DebugDescriptor, DebugProtocolError, DebugResponse, DiagnosticsRequest,
    decode_response, decode_rpc, encode_agent_request, encode_diagnostics_request, encode_rpc,
};
use endpoint_libs::libs::ws::mcp_wire::{
    JsonRpcId, JsonRpcMessage, JsonRpcRequest, MCP_PROTOCOL_VERSION,
};
use endpoint_libs::libs::ws::transport::{TransportStream, framed_json};
use endpoint_libs::libs::ws::{MessageStream, WireMessage};
use eyre::{Context, Result, bail, eyre};
use tokio::net::UnixStream;
use tokio::time::timeout;

/// Matches the Python client's bench timeout. Long because a driven
/// interaction can leave the app resolving for a while before it answers.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// Where an inspector announced itself, and what it said.
pub struct Descriptor {
    pub path: PathBuf,
    pub descriptor: DebugDescriptor,
    /// The descriptor verbatim, for the dump modes. Reprinting a re-serialized
    /// struct would hide any field this build of the tool does not know about.
    pub raw: serde_json::Value,
    verified_reachable: bool,
}

impl Descriptor {
    pub fn socket_path(&self) -> PathBuf {
        match self.descriptor.address.strip_prefix("unix://") {
            Some(path) => PathBuf::from(path),
            // The Python fell back to the descriptor path with the extension
            // swapped, and the server does name the socket that way.
            None => self.path.with_extension("sock"),
        }
    }

    /// Trap 8 in docs/performance.md: an unpinned descriptor directory keeps
    /// dead instances around. Pid existence is not enough: macOS reuses pids,
    /// so an unrelated process can make a stale descriptor look current. The
    /// control socket is the service, and a successful connection is the only
    /// liveness check that proves the descriptor can actually be used.
    fn is_reachable(&self) -> bool {
        std::os::unix::net::UnixStream::connect(self.socket_path()).is_ok()
    }

    pub fn warn_if_stale(&self) {
        if !self.verified_reachable && !self.is_reachable() {
            eprintln!(
                "warning: descriptor {} names pid {}, but its control socket is unreachable",
                self.path.display(),
                self.descriptor.pid
            );
        }
    }
}

/// Locate a running inspector, preferring an explicitly pinned descriptor.
///
/// `--descriptor <path>` wins. Otherwise the build's own pinned path is tried,
/// then the temporary directory is scanned, which is the fallback for a
/// hand-launched build and the one that can find a stale instance.
pub fn discover(explicit: Option<&str>) -> Result<Descriptor> {
    if let Some(path) = explicit {
        let path = PathBuf::from(path);
        if path.exists() {
            return read_descriptor(&path);
        }
    }

    // The delivery script pins this path into the bundle's `Info.plist`, so a
    // locally built app announces itself here and nowhere else. Scanning only
    // $TMPDIR meant the one instance that was definitely running was the one
    // instance discovery could not see, and it picked a dead descriptor from a
    // previous run instead — which is how preferring a live pid still failed.
    let pinned = PathBuf::from("target/blitz-control.json");
    if pinned.exists()
        && let Ok(mut descriptor) = read_descriptor(&pinned)
        && descriptor.is_reachable()
    {
        descriptor.verified_reachable = true;
        return Ok(descriptor);
    }

    let root = PathBuf::from(std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into()))
        .join("tauri-blitz-agent");
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
        .collect();
    found.sort();

    // Newest *reachable* instance, not simply newest.
    //
    // Descriptors outlive the process that wrote them, and a machine that has
    // run the app more than once has a directory full of them. Taking the most
    // recent file connected to whichever instance happened to exit last: at
    // best a refused connection, at worst a successful attach to a stale socket
    // and a set of numbers describing a process nobody is looking at. The
    // warning for that case already existed and was printed immediately before
    // connecting anyway.
    for (_, path) in found.iter().rev() {
        let Ok(mut descriptor) = read_descriptor(path) else {
            continue;
        };
        if descriptor.is_reachable() {
            descriptor.verified_reachable = true;
            return Ok(descriptor);
        }
    }

    bail!(
        "no reachable inspector descriptor found; is a diagnostics build running?\n\
         looked at target/blitz-control.json and {}. Pass --descriptor \
         <path> to inspect a specific descriptor.",
        root.display()
    )
}

fn read_descriptor(path: &Path) -> Result<Descriptor> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading descriptor {}", path.display()))?;
    let raw: serde_json::Value = serde_json::from_str(&text)
        .with_context(|| format!("parsing descriptor {}", path.display()))?;
    let descriptor: DebugDescriptor = serde_json::from_value(raw.clone())
        .with_context(|| format!("descriptor {} is not a DebugDescriptor", path.display()))?;
    Ok(Descriptor {
        path: path.to_path_buf(),
        descriptor,
        raw,
        verified_reachable: false,
    })
}

/// A connected inspector client.
///
/// `MessageStream` is the object-safe half of the endpoint-libs transport seam,
/// so the concrete `framed_json` type, which is opaque, never has to be named.
pub struct Client {
    stream: Box<dyn MessageStream>,
    next_id: i64,
    request_timeout: Duration,
}

impl Client {
    pub async fn connect(socket: &Path) -> Result<Self> {
        let stream = UnixStream::connect(socket)
            .await
            .with_context(|| format!("connecting to {}", socket.display()))?;
        Ok(Self {
            stream: Box::new(TransportStream::new(framed_json(stream))),
            next_id: 0,
            request_timeout: REQUEST_TIMEOUT,
        })
    }

    /// Bound every inspector exchange for a latency-sensitive command.
    ///
    /// Interactive dump/diagnostic modes retain the generous default. QA and
    /// coverage explicitly lower it so a dead action cannot multiply a
    /// minute-long transport wait across a suite.
    pub fn set_request_timeout(&mut self, request_timeout: Duration) {
        self.request_timeout = request_timeout;
    }

    fn next_id(&mut self) -> JsonRpcId {
        self.next_id += 1;
        JsonRpcId::Number(self.next_id)
    }

    /// Send one request and return the frame that answers *it*.
    ///
    /// Matching on the id is not pedantry. The server pushes notifications on
    /// the same socket, so a client that returns the next frame it sees will
    /// eventually hand a console message back as though it were metrics.
    async fn exchange(&mut self, request: WireMessage, id: &JsonRpcId) -> Result<WireMessage> {
        self.stream
            .send(request)
            .await
            .map_err(|error| eyre!("sending to the inspector failed: {error}"))?;
        loop {
            let message = timeout(self.request_timeout, self.stream.recv())
                .await
                .map_err(|_| {
                    eyre!(
                        "the inspector did not answer within {:?}",
                        self.request_timeout
                    )
                })?
                .ok_or_else(|| eyre!("the inspector closed the connection"))?
                .map_err(|error| eyre!("reading from the inspector failed: {error}"))?;
            if response_id(&message).as_ref() == Some(id) {
                return Ok(message);
            }
        }
    }

    /// A raw JSON-RPC call, for `initialize` and `tools/list`, which are not
    /// tool calls and so have no typed request in the protocol crate.
    pub async fn raw_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let id = self.next_id();
        let request = encode_rpc(JsonRpcMessage::Request(JsonRpcRequest::call(
            id.clone(),
            method,
            params,
        )))
        .map_err(protocol_error)?;
        let message = self.exchange(request, &id).await?;
        envelope(&message)
    }

    pub async fn initialize(&mut self) -> Result<serde_json::Value> {
        self.raw_request(
            "initialize",
            serde_json::json!({"protocolVersion": MCP_PROTOCOL_VERSION}),
        )
        .await
    }

    pub async fn tools_list(&mut self) -> Result<serde_json::Value> {
        self.raw_request("tools/list", serde_json::json!({})).await
    }

    /// An agent-control call, encoded from the server's own type.
    ///
    /// This is the whole reason the protocol crate exists. `AgentAction` is
    /// adjacently tagged, so the `{"action":"click","node_id":9}` that reads
    /// correctly is not what the server accepts, and getting it wrong used to
    /// present as a hung application rather than as an encoding mistake.
    pub async fn agent(&mut self, request: &AgentControlRequest) -> Result<Answer> {
        let id = self.next_id();
        let frame = encode_agent_request(id.clone(), request).map_err(protocol_error)?;
        let message = self.exchange(frame, &id).await?;
        Answer::new(message)
    }

    pub async fn diagnostics(&mut self, request: &DiagnosticsRequest) -> Result<Answer> {
        let id = self.next_id();
        let frame = encode_diagnostics_request(id.clone(), request).map_err(protocol_error)?;
        let message = self.exchange(frame, &id).await?;
        Answer::new(message)
    }

    /// The same call, but returning a protocol-level error rather than failing
    /// on it.
    ///
    /// `watch` needs this because `observe` is not implemented server-side: it
    /// answers `streamingUnavailable`. Printing that answer and then draining
    /// is what the previous client did, and it is the more useful behaviour —
    /// the mode reports what the server said instead of dying on it.
    pub async fn diagnostics_envelope(
        &mut self,
        request: &DiagnosticsRequest,
    ) -> Result<serde_json::Value> {
        let id = self.next_id();
        let frame = encode_diagnostics_request(id.clone(), request).map_err(protocol_error)?;
        let message = self.exchange(frame, &id).await?;
        envelope(&message)
    }

    /// Collect pushed notifications for a while, as `watch` does.
    pub async fn drain(&mut self, seconds: f64) -> Result<Vec<serde_json::Value>> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs_f64(seconds);
        let mut out = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(out);
            }
            match timeout(remaining, self.stream.recv()).await {
                Err(_) => return Ok(out),
                Ok(None) => return Ok(out),
                Ok(Some(Ok(message))) => out.push(envelope(&message)?),
                Ok(Some(Err(error))) => bail!("reading from the inspector failed: {error}"),
            }
        }
    }
}

/// A tool-call answer, kept in both forms.
///
/// The typed value is what every mode computes from. The envelope is what the
/// dump modes print, and printing a re-serialized struct instead would quietly
/// drop any field the server has gained since this binary was built.
pub struct Answer {
    pub envelope: serde_json::Value,
    pub response: DebugResponse,
}

impl Answer {
    fn new(message: WireMessage) -> Result<Self> {
        let envelope = envelope(&message)?;
        let (_, response) = decode_response(message).map_err(protocol_error)?;
        if let DebugResponse::Error(error) = &response {
            bail!("inspector returned {}: {}", error.code, error.message);
        }
        Ok(Self { envelope, response })
    }
}

fn envelope(message: &WireMessage) -> Result<serde_json::Value> {
    match message {
        WireMessage::Text(text) => {
            serde_json::from_str(text).context("the inspector sent a non-JSON text frame")
        }
        _ => bail!("the inspector sent a non-text frame"),
    }
}

fn response_id(message: &WireMessage) -> Option<JsonRpcId> {
    match decode_rpc(message.clone()) {
        Ok(JsonRpcMessage::Response(response)) => response.id,
        _ => None,
    }
}

fn protocol_error(error: DebugProtocolError) -> eyre::Report {
    eyre!("{error}")
}
