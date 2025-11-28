use tower_lsp_server as tower_lsp;
use clap::Parser;
use tower_lsp::lsp_types::*;
use tower_lsp::{LanguageServer, LspService, Server, jsonrpc};
use std::path::PathBuf;

mod backend;
use backend::Backend;
mod brace;
mod interpreter;
mod util;
use util::{logprint, LogType};
mod doc;

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult, jsonrpc::Error>
    {
        self.unpack_docs();
        self.load_docs().await;
        let m = &self.builtins_doc.lock().await.functions;
        logprint!(LogType::Info, "{}", serde_json::to_string_pretty(m).unwrap());
        // Thanks to Claude for helping with workspace root
        self.client.log_message(
            MessageType::LOG, format!("Workspace folders: {:#?}", params.workspace_folders.as_ref())
        ).await;
        let workspace_root = params.workspace_folders
            .as_ref()
            .and_then(|folders| folders.first())
            .and_then(|folder| {
                Some(PathBuf::from(folder.uri.path().as_str()))
            })
            .or_else(|| {
                #[allow(deprecated)]
                params.root_uri.and_then(|u| Some(PathBuf::from(u.path().as_str())))
            });

        if let Some(root) = &workspace_root {
            self.client.log_message(
                MessageType::INFO,
                format!("Workspace root: {:?}", root)
            ).await;
        }

        *self.workspace_root.lock().await = workspace_root;

        logprint!(LogType::Info, "Brenz initializing");
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                document_symbol_provider: Some(OneOf::Left(true)),
                hover_provider: Some(true.into()),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
                definition_provider: Some(OneOf::Left(true)),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams)
    {
        logprint!(LogType::Success, "Brenz initialized");
        self.client.log_message(MessageType::INFO, "Brenz: Server Initialized").await;
        logprint!(LogType::Info, "Workspace: {:?}", self.workspace_root.lock().await);
    }

    async fn shutdown(&self) -> Result<(), tower_lsp::jsonrpc::Error>
    {
        self.client.log_message(MessageType::INFO, "Brenz: Server Shutdown").await;
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams)
    {
        self.client.log_message(MessageType::INFO,format!("Opened: {}", params.text_document.uri.as_str())).await;

        // Parse the newly opened document
        self.parse_and_diagnose(params.text_document.uri.clone(), &params.text_document.text).await;

        self.docs_content.lock().await.insert(params.text_document.uri, params.text_document.text);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams)
    {
        // For full sync, we get the entire document content
        if let Some(change) = params.content_changes.first() {
            self.client.log_message(MessageType::INFO, format!("Changed: {}", params.text_document.uri.as_str())).await;

            // Re-parse the changed document
            self.parse_and_diagnose(params.text_document.uri.clone(), &change.text).await;
            let mut dc = self.docs_content.lock().await;

            // https://stackoverflow.com/a/30414450
            if dc.contains_key(&params.text_document.uri) {
                *dc.get_mut(&params.text_document.uri).unwrap() = change.text.clone();
            }
            else {
                dc.insert(params.text_document.uri, change.text.clone());
            }
        }
    }

    async fn did_save(&self, _params: DidSaveTextDocumentParams)
    {
        //log_print(&format!("Saved: {}", params.text_document.uri.as_str()));
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams)
    {
        self.trees.lock().await.remove(&params.text_document.uri);
        self.docs_content.lock().await.remove(&params.text_document.uri);
        self.client.publish_diagnostics(params.text_document.uri, vec![], None).await;
        // not clearing the fn_defs for this file
    }

    async fn goto_definition(
        &self, params: GotoDefinitionParams
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>>
    {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let (tree, src) = {
            let trees = self.trees.lock().await;
            let tree = match trees.get(&uri) {
                Some(t) => t.clone(),
                None => return Ok(None),
            };

            let dc = self.docs_content.lock().await;
            let src = match dc.get(&uri) {
                Some(s) => s.clone(),
                None => return Ok(None),
            };

            (tree, src)
        };

        let node = self.node_at_pos(&tree, &src, pos).unwrap();
        //self.client.log_message(MessageType::INFO, format!("kind: {}, field: {}", node.kind(), node.parent().unwrap().kind())).await;
        if node.kind() != "identifier" {
            return Ok(None);
        }

        if let Some(fn_node) = self.find_parent_of_kind(node, "direct_call")
            .or_else(|| self.find_parent_of_kind(node, "thread_call"))
            .or_else(|| self.find_parent_of_kind(node, "object_call"))
            .or_else(|| self.find_parent_of_kind(node, "function_pointer"))
        {
            logprint!(LogType::Info, "in a call_node");
            if let Some(res) = self.resolve_target_fn(fn_node, &src, &uri, false).await {
                return Ok(Some(GotoDefinitionResponse::Scalar(res.0)));
            }
        }

        let identifier = &src[node.start_byte()..node.end_byte()];
        if let Some(assignment_node) = self.find_var_def(identifier, node, &src) {
            let range = Range {
                start: self.byte_to_position(&src, assignment_node.start_byte()),
                end: self.byte_to_position(&src, assignment_node.end_byte()),
            };
            return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                uri: uri,
                range: range
            })));
        }

        Ok(None)
    }

    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>>
    {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let (tree, src) = {
            let trees = self.trees.lock().await;
            let tree = match trees.get(&uri) {
                Some(t) => t.clone(),
                None => return Ok(None),
            };

            let dc = self.docs_content.lock().await;
            let src = match dc.get(&uri) {
                Some(s) => s.clone(),
                None => return Ok(None),
            };

            (tree, src)
        };

        let node = self.node_at_pos(&tree, &src, pos).unwrap();
        match node.kind() {
            "wait" => {
                let txt = doc::strings::WAIT_INFO.to_string();
                return Ok(Some(Hover {
                    contents: HoverContents::Scalar(MarkedString::String(txt)),
                               range: None
                }));
            }
            "thread" => {
                let txt = doc::strings::THREAD_INFO.to_string();
                return Ok(Some(Hover {
                    contents: HoverContents::Scalar(MarkedString::String(txt)),
                               range: None
                }));
            }
            _ => {}
        }

        // Only identifiers can go beyond this point
        if node.kind() != "identifier" {
            let strr = &src[node.start_byte()..node.end_byte()];
            self.client.log_message(MessageType::LOG, &format!("{}, {}", strr, node.kind())).await;
            return Ok(None);
        }

        let identifier = &src[node.start_byte()..node.end_byte()];
        self.client.log_message(MessageType::LOG, &format!("identifier: {}", identifier)).await;
        match identifier {
            "self" => {
                let txt = doc::strings::SELF_INFO.to_string();
                return Ok(Some(Hover {
                    contents: HoverContents::Scalar(MarkedString::String(txt)),
                               range: None
                }));
            }
            "level" => {
                let txt = doc::strings::LEVEL_INFO.to_string();
                return Ok(Some(Hover {
                    contents: HoverContents::Scalar(MarkedString::String(txt)),
                               range: None
                }));
            }
            "game" => {
                let txt = doc::strings::GAME_INFO.to_string();
                return Ok(Some(Hover {
                    contents: HoverContents::Scalar(MarkedString::String(txt)),
                               range: None
                }));
            }
            _ => {}
        }

        if let Some(call_node) = self.find_parent_of_kind(node, "direct_call")
            .or_else(|| self.find_parent_of_kind(node, "thread_call"))
            .or_else(|| self.find_parent_of_kind(node, "object_call"))
            .or_else(|| self.find_parent_of_kind(node, "function_pointer"))
        {
            logprint!(LogType::Info, "(hover) in a call_node");
            match self.resolve_target_fn(call_node, &src, &uri, true).await {
                Some(res) => {
                    let txt = self.hover_info(res, uri).await
                        .unwrap_or(String::from("Failed to get hover info"));
                    return Ok(Some(Hover {
                        contents: HoverContents::Scalar(MarkedString::String(txt)),
                        range: None
                    }));
                }
                None => {
                    logprint!(LogType::Error, "Failed to resolve_target_fn");
                }
            }
        }

        Ok(Some(Hover {
            contents: HoverContents::Scalar(MarkedString::String(
                "We're not there yet.\n[Contribute :)](https://gitlab.com/kazam0180/brenz)".to_string()
            )),
            range: None
        }))
    }

    async fn document_symbol(
        &self, params: DocumentSymbolParams
    ) ->jsonrpc::Result<Option<DocumentSymbolResponse>>
    {
        let uri = params.text_document.uri;
        let symbols = match self.get_syms(&uri).await {
            Some(syms) => {
                logprint!(LogType::Info, "Symobols: {:#?}", syms.clone());
                syms
            }
            None => {
                logprint!(LogType::Error, "Failed to get document symbols for file: {}", uri.path().as_str());
                Vec::new()
            }
        };

        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }
}

#[derive(Parser)]
struct Args
{
    /// Initialize a project in this folder
    #[clap(long, short, action)]
    init: bool,
    /// Enable backtrace for debugging
    #[clap(long, short, action)]
    backtrace: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    if args.backtrace {
        unsafe { std::env::set_var("RUST_BACKTRACE", "1"); }
    }
    if args.init {
        match util::init_cfg() {
            Ok(p) => {
                logprint!(
                    LogType::Success, "Initialized a project in: {}",
                    p.to_str().unwrap()
                );
            }
            Err(e) => {
                logprint!(LogType::Error, "Failed to initialize project: {e}");
            }
        }
        std::process::exit(0);
    }
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| Backend::new(client));
    Server::new(stdin, stdout, socket).serve(service).await;
}
