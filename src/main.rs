use async_trait::async_trait;
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

#[async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult, jsonrpc::Error>
    {
        // Thanks to Claude for helping with workspace root
        let workspace_root = params.root_uri
        .as_ref()
        .and_then(|uri| Some(PathBuf::from(uri.path())))
        .or_else(|| {
            params.root_uri
            .as_ref()
            .map(|p| PathBuf::from(p.path()))
        })
        .or_else(|| {
            logprint!(LogType::Info, "using fallback workspace");
            // Try workspace_folders as fallback
            params.workspace_folders
            .as_ref()
            .and_then(|folders| folders.first())
            .and_then(|folder| PathBuf::from(folder.uri.path()).parent().map(|p| p.to_path_buf()))
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
    }

    async fn shutdown(&self) -> Result<(), tower_lsp::jsonrpc::Error>
    {
        self.client.log_message(MessageType::INFO, "Brenz: Server Shutdown").await;
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams)
    {
        self.client.log_message(MessageType::INFO,format!("Opened: {}", params.text_document.uri)).await;

        // Parse the newly opened document
        self.parse_and_diagnose(params.text_document.uri.clone(), &params.text_document.text).await;

        self.docs_content.lock().await.insert(params.text_document.uri, params.text_document.text);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams)
    {
        // For full sync, we get the entire document content
        if let Some(change) = params.content_changes.first() {
            self.client.log_message(MessageType::INFO, format!("Changed: {}", params.text_document.uri)).await;

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
    }

    async fn goto_definition(
        &self, params: GotoDefinitionParams
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>>
    {
        let uri = params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;

        let trees = self.trees.lock().await;
        let tree = match trees.get(&uri) {
            Some(t) => t,
            None => return Ok(None),
        };

        let dc = self.docs_content.lock().await;
        let src = dc.get(&uri).unwrap();

        let node = self.node_at_pos(tree, src, pos).unwrap();
        //self.client.log_message(MessageType::INFO, format!("kind: {}, field: {}", node.kind(), node.parent().unwrap().kind())).await;
        if node.kind() != "identifier" {
            return Ok(None);
        }

        if let Some(direct_call) = self.find_parent_of_kind(node, "direct_call") {
            //self.client.log_message(MessageType::INFO, format!("me: {}, child: {:#?}", direct_call.kind(), direct_call.child_by_field_name("path"))).await;

            // Direct call to foreign function
            if let Some(foreign_ref) = self.find_child_of_kind(direct_call, "foreign_function_ptr") {
                logprint!(LogType::Info, "foreign");
                match self.process_foreign_fn(foreign_ref, src) {
                    Some((p, s, f)) => {
                        logprint!(LogType::Info, "past processing: {:#?}, {}, {}", &p, &s, &f);
                        let root = self.workspace_root.lock().await;
                        match util::resolove_scr_path(&root, p, s) {
                            Ok(r) => {
                                logprint!(LogType::Info, "past resolove_path: {:#?}", &r);
                                let s_uri = Url::from_file_path(r).unwrap();
                                let fn_defs = self.fn_defs.lock().await;
                                if let Some(funcs) = fn_defs.get(&s_uri) {
                                    if let Some(range) = funcs.get(&f) {
                                        return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                                            uri: s_uri,
                                            range: *range,
                                        })));
                                    }
                                }
                            },
                            Err(e) => {
                                util::logprint!(LogType::Error, "Can't determine script path: {e}");
                                self.client.log_message(MessageType::ERROR, format!("Can't determine script path: {e}")).await;
                            }
                        };
                    }
                    None => {}
                };
            }

            // Direct call to local function
            if let Some(func) = direct_call.child_by_field_name("function") {
                let fn_name = &src[func.start_byte()..func.end_byte()];
                let fn_defs = self.fn_defs.lock().await;
                if let Some(funcs) = fn_defs.get(&uri) {
                    if let Some(range) = funcs.get(fn_name) {
                        return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                            uri: uri,
                            range: *range,
                        })));
                    }
                }
            }
        }
        //self.client.log_message(MessageType::INFO, format!("functions: {:#?}", self.fn_defs.lock().await)).await;

        Ok(None)
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
