//! The main loop.
//!
//! Messages are handled one at a time on this thread, which owns the database
//! outright: no locks, no shared mutable state, and no chance of a request
//! observing a half-applied edit. Responsiveness comes from two choices
//! instead: background indexing runs in small slices only while the message
//! queue is empty, and diagnostics are computed at the same quiet points, so a
//! burst of keystrokes produces one diagnostics pass rather than one per key.

use crate::capabilities::server_capabilities;
use crate::handlers;
use crate::state::State;
use lsp_server::{Connection, Message, Notification};
use lsp_types::notification::PublishDiagnostics;
use lsp_types::{InitializeParams, ServerInfo};
use project_model::Config;
use std::path::PathBuf;
use std::time::Instant;

pub fn run(connection: Connection) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let started = Instant::now();
    let (id, params) = connection.initialize_start()?;
    let params: InitializeParams = serde_json::from_value(params)?;

    let root = workspace_root(&params);
    let mut config = root
        .as_deref()
        .map(project_model::load_config_file)
        .unwrap_or_default();
    // Client settings win over the file on disk.
    if let Some(options) = &params.initialization_options {
        match serde_json::from_value::<Config>(options.clone()) {
            Ok(from_client) => merge(&mut config, from_client),
            Err(error) => {
                tracing::warn!(%error, "ignoring unreadable initializationOptions")
            }
        }
    }

    let text_document = params.capabilities.text_document.as_ref();
    let supports_rename_prepare = text_document
        .and_then(|caps| caps.rename.as_ref())
        .and_then(|rename| rename.prepare_support)
        .unwrap_or(false);
    let supports_snippets = text_document
        .and_then(|caps| caps.completion.as_ref())
        .and_then(|completion| completion.completion_item.as_ref())
        .and_then(|item| item.snippet_support)
        .unwrap_or(false);
    let supports_additional_edits = text_document
        .and_then(|caps| caps.completion.as_ref())
        .and_then(|completion| completion.completion_item.as_ref())
        .and_then(|item| item.insert_replace_support.or(Some(true)))
        .unwrap_or(true);
    let supports_location_links = text_document
        .and_then(|caps| caps.definition.as_ref())
        .and_then(|definition| definition.link_support)
        .unwrap_or(false);

    let initialize_result = serde_json::json!({
        "capabilities": server_capabilities(supports_rename_prepare),
        "serverInfo": ServerInfo {
            name: "ferrisjdt".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        },
    });
    connection.initialize_finish(id, initialize_result)?;

    let root = root.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let mut state = State::new(&root, config);
    state.completion_settings = java_ide::completion::CompletionSettings {
        snippets: supports_snippets,
        additional_edits: supports_additional_edits,
    };
    state.supports_location_links = supports_location_links;
    tracing::info!(
        startup_ms = started.elapsed().as_millis(),
        files = state.pending_count(),
        "ready"
    );

    main_loop(&connection, &mut state)
}

fn main_loop(
    connection: &Connection,
    state: &mut State,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    loop {
        // Drain the queue first: a pending message always takes priority over
        // background work, which is what keeps typing latency low.
        match connection.receiver.try_recv() {
            Ok(message) => {
                if handle(connection, state, message)? {
                    return Ok(());
                }
                continue;
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => return Ok(()),
            Err(crossbeam_channel::TryRecvError::Empty) => {}
        }

        // The queue is empty, so this is the moment for deferred work.
        if state.index_slice() {
            continue;
        }
        let dirty = state.take_dirty();
        if !dirty.is_empty() {
            for file in dirty {
                if let Some(params) = handlers::publish_for(state, file) {
                    send_notification::<PublishDiagnostics>(connection, params);
                }
            }
            continue;
        }

        // Nothing left to do: block until the client speaks, so an idle
        // server uses no CPU at all.
        match connection.receiver.recv() {
            Ok(message) => {
                if handle(connection, state, message)? {
                    return Ok(());
                }
            }
            Err(_) => return Ok(()),
        }
    }
}

/// Handles one message. Returns true when the server should exit.
fn handle(
    connection: &Connection,
    state: &mut State,
    message: Message,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    match message {
        Message::Request(request) => {
            if connection.handle_shutdown(&request)? {
                return Ok(true);
            }
            // A request the client already cancelled is answered with the
            // protocol's cancellation error rather than a stale result. The
            // check happens here, before any analysis, because that is where
            // a queued cancellation can still save the work.
            if state.is_cancelled(&request.id) {
                connection
                    .sender
                    .send(Message::Response(lsp_server::Response::new_err(
                        request.id,
                        lsp_server::ErrorCode::RequestCanceled as i32,
                        "request cancelled".to_string(),
                    )))?;
                return Ok(false);
            }
            let response = handlers::handle_request(state, request);
            connection.sender.send(Message::Response(response))?;
        }
        Message::Notification(notification) => {
            handlers::handle_notification(state, notification);
        }
        Message::Response(_) => {}
    }
    Ok(false)
}

fn send_notification<N>(connection: &Connection, params: N::Params)
where
    N: lsp_types::notification::Notification,
    N::Params: serde::Serialize,
{
    let notification = Notification {
        method: N::METHOD.to_string(),
        params: serde_json::to_value(params).unwrap_or(serde_json::Value::Null),
    };
    if let Err(error) = connection.sender.send(Message::Notification(notification)) {
        tracing::warn!(%error, "failed to send notification");
    }
}

/// The workspace root, from the folders the client sent or its root URI.
fn workspace_root(params: &InitializeParams) -> Option<PathBuf> {
    if let Some(folders) = &params.workspace_folders {
        if let Some(first) = folders.first() {
            if let Some(path) = java_ide::uri::to_path(&first.uri) {
                return Some(path);
            }
        }
    }
    #[allow(deprecated)] // root_uri is deprecated but still what many clients send.
    if let Some(uri) = &params.root_uri {
        if let Some(path) = java_ide::uri::to_path(uri) {
            return Some(path);
        }
    }
    None
}

/// Applies client-provided settings over whatever the config file set.
fn merge(target: &mut Config, from_client: Config) {
    if from_client.jdk_home.is_some() {
        target.jdk_home = from_client.jdk_home;
    }
    if from_client.source_level.is_some() {
        target.source_level = from_client.source_level;
    }
    if from_client.max_source_files.is_some() {
        target.max_source_files = from_client.max_source_files;
    }
    target.source_roots.extend(from_client.source_roots);
    target.classpath.extend(from_client.classpath);
    target.exclude.extend(from_client.exclude);
}
