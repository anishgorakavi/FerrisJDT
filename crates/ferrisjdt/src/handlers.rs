//! Request and notification handlers.

use crate::state::State;
use java_ide::{
    code_actions, completion, diagnostics, hover, navigation, rename, signature, symbols, tokens,
};
use java_semantic::symbols::FileId;
use lsp_server::{ErrorCode, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidChangeWatchedFiles, DidCloseTextDocument, DidOpenTextDocument,
    DidSaveTextDocument, Notification as _,
};
use lsp_types::request::{
    CodeActionRequest, Completion, DocumentHighlightRequest, DocumentSymbolRequest, GotoDefinition,
    GotoImplementation, GotoTypeDefinition, HoverRequest, PrepareRenameRequest, References, Rename,
    Request as _, ResolveCompletionItem, SelectionRangeRequest, SemanticTokensFullRequest,
    SignatureHelpRequest, WorkspaceSymbolRequest,
};
use lsp_types::{
    CompletionResponse, DocumentSymbolResponse, FileChangeType, GotoDefinitionResponse,
    PrepareRenameResponse, SemanticTokensResult, WorkspaceSymbolResponse,
};
use serde::Serialize;
use std::time::Instant;

/// Dispatches one request, returning the response to send.
pub fn handle_request(state: &mut State, request: Request) -> Response {
    let started = Instant::now();
    // Resolution reaches across the whole workspace, so answering before the
    // index is built would produce a confidently wrong result — a missing
    // completion or a definition that goes nowhere. Finishing the index first
    // makes the first request slower and every request correct.
    if state.is_indexing() {
        let pending = state.pending_count();
        while state.index_slice() {}
        tracing::info!(
            pending,
            elapsed_ms = started.elapsed().as_millis(),
            "finished indexing before serving a request"
        );
    }
    let method = request.method.clone();
    let id = request.id.clone();
    let response = dispatch(state, request);
    // Timings go to stderr; they are how the latency targets are checked
    // against real editor traffic rather than only against benchmarks.
    tracing::debug!(method = %method, elapsed_us = started.elapsed().as_micros(), "request");
    let _ = id;
    response
}

fn dispatch(state: &mut State, request: Request) -> Response {
    let id = request.id.clone();
    match request.method.as_str() {
        Completion::METHOD => with_params::<Completion, _>(state, request, |state, params| {
            let position = params.text_document_position;
            let (file, offset) = locate(state, &position)?;
            let settings = state.completion_settings;
            let items = completion::completions_repairing(&mut state.db, file, offset, settings);
            // `isIncomplete` keeps the client asking as the prefix grows,
            // which matters because the classpath list is capped per request.
            Some(CompletionResponse::List(lsp_types::CompletionList {
                is_incomplete: true,
                items,
            }))
        }),
        ResolveCompletionItem::METHOD => {
            with_params::<ResolveCompletionItem, _>(state, request, |state, item| {
                completion::resolve(&state.db, item)
            })
        }
        HoverRequest::METHOD => with_params::<HoverRequest, _>(state, request, |state, params| {
            let (file, offset) = locate(state, &params.text_document_position_params)?;
            hover::hover(&state.db, file, offset)
        }),
        GotoDefinition::METHOD => {
            with_params::<GotoDefinition, _>(state, request, |state, params| {
                let (file, offset) = locate(state, &params.text_document_position_params)?;
                let links = navigation::definition(&state.db, file, offset);
                (!links.is_empty()).then(|| as_goto_response(state, links))
            })
        }
        GotoTypeDefinition::METHOD => {
            with_params::<GotoTypeDefinition, _>(state, request, |state, params| {
                let (file, offset) = locate(state, &params.text_document_position_params)?;
                let links = navigation::type_definition(&state.db, file, offset);
                (!links.is_empty()).then(|| as_goto_response(state, links))
            })
        }
        GotoImplementation::METHOD => {
            with_params::<GotoImplementation, _>(state, request, |state, params| {
                let (file, offset) = locate(state, &params.text_document_position_params)?;
                Some(GotoDefinitionResponse::Array(navigation::implementations(
                    &state.db, file, offset,
                )))
            })
        }
        References::METHOD => with_params::<References, _>(state, request, |state, params| {
            let (file, offset) = locate(state, &params.text_document_position)?;
            Some(navigation::references(
                &state.db,
                file,
                offset,
                params.context.include_declaration,
            ))
        }),
        DocumentHighlightRequest::METHOD => {
            with_params::<DocumentHighlightRequest, _>(state, request, |state, params| {
                let (file, offset) = locate(state, &params.text_document_position_params)?;
                Some(navigation::document_highlights(&state.db, file, offset))
            })
        }
        DocumentSymbolRequest::METHOD => {
            with_params::<DocumentSymbolRequest, _>(state, request, |state, params| {
                let file = state.file_for(&params.text_document.uri)?;
                Some(DocumentSymbolResponse::Nested(symbols::document_symbols(
                    &state.db, file,
                )))
            })
        }
        WorkspaceSymbolRequest::METHOD => {
            with_params::<WorkspaceSymbolRequest, _>(state, request, |state, params| {
                Some(WorkspaceSymbolResponse::Nested(symbols::workspace_symbols(
                    &state.db,
                    &params.query,
                )))
            })
        }
        SignatureHelpRequest::METHOD => {
            with_params::<SignatureHelpRequest, _>(state, request, |state, params| {
                let (file, offset) = locate(state, &params.text_document_position_params)?;
                signature::signature_help(&state.db, file, offset)
            })
        }
        CodeActionRequest::METHOD => {
            with_params::<CodeActionRequest, _>(state, request, |state, params| {
                let file = state.file_for(&params.text_document.uri)?;
                Some(code_actions::code_actions(
                    &state.db,
                    file,
                    params.range,
                    &params.context.diagnostics,
                ))
            })
        }
        PrepareRenameRequest::METHOD => {
            with_params::<PrepareRenameRequest, _>(state, request, |state, params| {
                let (file, offset) = locate(state, &params)?;
                let (range, placeholder) = navigation::rename_placeholder(&state.db, file, offset)?;
                Some(PrepareRenameResponse::RangeWithPlaceholder { range, placeholder })
            })
        }
        Rename::METHOD => {
            let params = match serde_json::from_value::<lsp_types::RenameParams>(request.params) {
                Ok(params) => params,
                Err(error) => return invalid_params(id, error),
            };
            let Some((file, offset)) = locate(state, &params.text_document_position) else {
                return Response::new_err(
                    id,
                    ErrorCode::InvalidParams as i32,
                    "unknown document".to_string(),
                );
            };
            match rename::rename(&state.db, file, offset, &params.new_name) {
                Ok(edit) => Response::new_ok(id, edit),
                // A refused rename is reported as an error so the editor shows
                // the reason instead of silently applying nothing.
                Err(error) => {
                    Response::new_err(id, ErrorCode::RequestFailed as i32, error.to_string())
                }
            }
        }
        SelectionRangeRequest::METHOD => {
            with_params::<SelectionRangeRequest, _>(state, request, |state, params| {
                let file = state.file_for(&params.text_document.uri)?;
                let ranges = params
                    .positions
                    .into_iter()
                    .filter_map(|position| {
                        let offset = java_ide::convert::offset_of(&state.db, file, position)?;
                        symbols::selection_ranges(&state.db, file, offset)
                    })
                    .collect::<Vec<_>>();
                Some(ranges)
            })
        }
        SemanticTokensFullRequest::METHOD => {
            with_params::<SemanticTokensFullRequest, _>(state, request, |state, params| {
                let file = state.file_for(&params.text_document.uri)?;
                tokens::semantic_tokens(&state.db, file).map(SemanticTokensResult::Tokens)
            })
        }
        crate::status::METHOD => {
            let status = crate::status::status(state);
            Response::new_ok(id, status)
        }
        other => Response::new_err(
            id,
            ErrorCode::MethodNotFound as i32,
            format!("unsupported request: {other}"),
        ),
    }
}

/// Deserializes a request's parameters and serializes the handler's result.
///
/// The handler yields `R::Result` directly. Most LSP result types are already
/// `Option<..>`, where `None` serializes to null and means "nothing applies" —
/// a valid answer, distinct from an error.
fn with_params<R, F>(state: &mut State, request: Request, handler: F) -> Response
where
    R: lsp_types::request::Request,
    R::Params: serde::de::DeserializeOwned,
    R::Result: Serialize,
    F: FnOnce(&mut State, R::Params) -> R::Result,
{
    let id = request.id.clone();
    match serde_json::from_value::<R::Params>(request.params) {
        Ok(params) => Response::new_ok(id, handler(state, params)),
        Err(error) => invalid_params(id, error),
    }
}

fn invalid_params(id: RequestId, error: serde_json::Error) -> Response {
    Response::new_err(
        id,
        ErrorCode::InvalidParams as i32,
        format!("invalid parameters: {error}"),
    )
}

/// Clients that understand `LocationLink` get the richer form, which
/// highlights the origin range; the rest get plain locations.
fn as_goto_response(state: &State, links: Vec<lsp_types::LocationLink>) -> GotoDefinitionResponse {
    if state.supports_location_links {
        GotoDefinitionResponse::Link(links)
    } else {
        GotoDefinitionResponse::Array(
            links
                .into_iter()
                .map(|link| lsp_types::Location {
                    uri: link.target_uri,
                    range: link.target_selection_range,
                })
                .collect(),
        )
    }
}

/// Resolves a text document position to a file and byte offset.
fn locate(
    state: &mut State,
    position: &lsp_types::TextDocumentPositionParams,
) -> Option<(FileId, u32)> {
    let file = state.file_for(&position.text_document.uri)?;
    let offset = java_ide::convert::offset_of(&state.db, file, position.position)?;
    Some((file, offset))
}

/// Handles a notification. Returns the files whose diagnostics should be
/// recomputed.
pub fn handle_notification(state: &mut State, notification: lsp_server::Notification) {
    match notification.method.as_str() {
        DidOpenTextDocument::METHOD => {
            if let Ok(params) =
                serde_json::from_value::<lsp_types::DidOpenTextDocumentParams>(notification.params)
            {
                let Some(path) = java_ide::uri::to_path(&params.text_document.uri) else {
                    return;
                };
                let file = state.db.open_document(
                    &path,
                    params.text_document.text,
                    params.text_document.version,
                );
                state.mark_dirty(file);
            }
        }
        DidChangeTextDocument::METHOD => {
            if let Ok(params) = serde_json::from_value::<lsp_types::DidChangeTextDocumentParams>(
                notification.params,
            ) {
                let Some(file) = state.file_for(&params.text_document.uri) else {
                    return;
                };
                let version = params.text_document.version;
                state.db.edit_document(file, version, |source| {
                    for change in params.content_changes {
                        match change.range {
                            Some(range) => source.apply_change(
                                java_ide::convert::to_syntax_position(range.start),
                                java_ide::convert::to_syntax_position(range.end),
                                &change.text,
                                version,
                            ),
                            None => source.set_text(change.text, version),
                        }
                    }
                });
                state.mark_dirty(file);
            }
        }
        DidSaveTextDocument::METHOD => {
            if let Ok(params) =
                serde_json::from_value::<lsp_types::DidSaveTextDocumentParams>(notification.params)
            {
                if let Some(file) = state.file_for(&params.text_document.uri) {
                    state.mark_dirty(file);
                }
            }
        }
        DidCloseTextDocument::METHOD => {
            if let Ok(params) =
                serde_json::from_value::<lsp_types::DidCloseTextDocumentParams>(notification.params)
            {
                if let Some(file) = state.file_for(&params.text_document.uri) {
                    state.db.close_document(file);
                }
            }
        }
        DidChangeWatchedFiles::METHOD => {
            if let Ok(params) = serde_json::from_value::<lsp_types::DidChangeWatchedFilesParams>(
                notification.params,
            ) {
                for change in params.changes {
                    let Some(path) = java_ide::uri::to_path(&change.uri) else {
                        continue;
                    };
                    if !path.extension().is_some_and(|e| e == "java") {
                        continue;
                    }
                    match change.typ {
                        FileChangeType::DELETED => {
                            if let Some(file) = state.db.file_id(&path) {
                                state.db.remove_file(file);
                            }
                        }
                        _ => {
                            if let Some(file) = state.db.load_from_disk(&path) {
                                state.mark_dirty(file);
                            }
                        }
                    }
                }
            }
        }
        "$/cancelRequest" => {
            if let Ok(params) =
                serde_json::from_value::<lsp_types::CancelParams>(notification.params)
            {
                match params.id {
                    lsp_types::NumberOrString::Number(id) => {
                        state.cancel(lsp_server::RequestId::from(id))
                    }
                    lsp_types::NumberOrString::String(id) => {
                        state.cancel(lsp_server::RequestId::from(id))
                    }
                }
            }
        }
        other => tracing::debug!(method = other, "ignoring notification"),
    }
}

/// Computes diagnostics for a file.
pub fn publish_for(state: &State, file: FileId) -> Option<lsp_types::PublishDiagnosticsParams> {
    let uri = java_ide::convert::uri_of(&state.db, file)?;
    let started = Instant::now();
    let items = diagnostics::diagnostics(&state.db, file);
    tracing::debug!(
        count = items.len(),
        elapsed_us = started.elapsed().as_micros(),
        "diagnostics"
    );
    Some(lsp_types::PublishDiagnosticsParams {
        uri,
        diagnostics: items,
        version: state.db.version(file),
    })
}
