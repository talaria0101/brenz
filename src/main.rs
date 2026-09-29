use tower_lsp_server as tower_lsp;
use clap::Parser;
use tower_lsp::lsp_types::*;
use tower_lsp::{LanguageServer, LspService, Server, jsonrpc};
use tokio::time::{Duration, Instant};
use std::path::PathBuf;

mod backend;
use backend::Backend;
mod brace;
mod compiler;
mod util;
use util::{logprint, LogType};
mod doc;

impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult, jsonrpc::Error>
    {
        self.unpack_docs();
        self.load_docs().await;
        let (f, m) = {
            let b = self.builtins_doc.lock().await;
            (b.functions.clone(), b.methods.clone())
        };
        logprint!(LogType::Info, "Builtin functions: \n{}", serde_json::to_string_pretty(&f).unwrap());
        logprint!(LogType::Info, "Builtin methods: \n{}", serde_json::to_string_pretty(&m).unwrap());
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
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(false),
                    trigger_characters: Some(vec![".".to_string()]),
                    ..Default::default()
                }),
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
        let checker = self.checker.clone();
        let parser = self.parser.clone();
        let trees = self.trees.clone();
        let fn_defs = self.fn_defs.clone();
        let sym_defs = self.sym_defs.clone();
        let client = self.client.clone();
        Self::parse_and_diagnose(
            params.text_document.uri.clone(),
            &params.text_document.text,
            checker,
            parser,
            trees,
            fn_defs,
            sym_defs,
            client
        ).await;

        self.docs_content.lock().await.insert(params.text_document.uri, params.text_document.text);
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams)
    {
        // For full sync, we get the entire document content
        if let Some(change) = params.content_changes.first() {
            self.client.log_message(MessageType::INFO, format!("Changed: {}", params.text_document.uri.as_str())).await;

            let uri = params.text_document.uri.clone();
            let text = change.text.clone();
            let now = Instant::now();

            self.docs_content.lock().await.insert(params.text_document.uri, change.text.clone());
            self.last_edit_time.lock().await.insert(uri.clone(), now);

            let checker = self.checker.clone();
            let parser = self.parser.clone();
            let trees = self.trees.clone();
            let fn_defs = self.fn_defs.clone();
            let sym_defs = self.sym_defs.clone();
            let client = self.client.clone();
            let last_edit = self.last_edit_time.clone();

            tokio::spawn(async move {
                // wait 300 ms
                tokio::time::sleep(Duration::from_millis(300)).await;

                let last = last_edit.lock().await.get(&uri).copied();
                if last == Some(now) {
                    Self::parse_and_diagnose(
                        uri.clone(),
                        &text,
                        checker,
                        parser,
                        trees,
                        fn_defs,
                        sym_defs,
                        client
                    ).await;
                }
            });
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
        self.last_edit_time.lock().await.remove(&params.text_document.uri);
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
                start: Self::byte_to_position(&src, assignment_node.start_byte()),
                end: Self::byte_to_position(&src, assignment_node.end_byte()),
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
            "wait" | "thread" => {
                if let Some(info) = self.identifier_hover_info(&src[node.start_byte()..node.end_byte()]) {
                    return Ok(Some(info));
                }
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

        if let Some(info) = self.identifier_hover_info(identifier) {
            return Ok(Some(info));
        }

        if let Some(call_node) = self.find_parent_of_kind(node, "direct_call")
            .or_else(|| self.find_parent_of_kind(node, "thread_call"))
            .or_else(|| self.find_parent_of_kind(node, "object_call"))
            .or_else(|| self.find_parent_of_kind(node, "function_pointer"))
        {
            logprint!(LogType::Info, "(hover) in a call_node");
            let builtin_info: String = {
                let r = self.builtins_doc.lock().await;
                logprint!(LogType::Info, "The identifier in question: {}", &identifier.to_lowercase());
                logprint!(LogType::Info, "Builtin functions: {:#?}", r.functions.keys());
                let b = r.methods.get(&identifier.to_ascii_lowercase());

                if let Some(b) = b {
                    Self::info_from_builtin(b, &b.sign)
                }
                else {
                    let b = r.functions.get(&identifier.to_ascii_lowercase());
                    if let Some(b) = b {
                        Self::info_from_builtin(b, &b.sign)
                    }
                    else { String::new() }
                }
            };
            if !builtin_info.is_empty() {
                logprint!(LogType::Info, "txt: {}", &builtin_info);
                return Ok(Some(Hover {
                    contents: HoverContents::Scalar(MarkedString::String(builtin_info)),
                    range: None
                }));
            }

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
                //logprint!(LogType::Info, "Symobols: {:#?}", syms.clone());
                syms
            }
            None => {
                logprint!(LogType::Error, "Failed to get document symbols for file: {}", uri.path().as_str());
                Vec::new()
            }
        };

        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    async fn completion(&self, params: CompletionParams) -> jsonrpc::Result<Option<CompletionResponse>>
    {
        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

        let src = {
            /*let trees = self.trees.lock().await;
            let tree = match trees.get(&uri) {
                Some(t) => t.clone(),
                None => return Ok(None),
            };*/

            let dc = self.docs_content.lock().await;
            let src = match dc.get(&uri) {
                Some(s) => s.clone(),
                None => return Ok(None),
            };

            src
            //(tree, src)
        };

        // Check if triggered by a character
        let trigger = params.context.and_then(|ctx| ctx.trigger_character);

        eprintln!("trigger: {:?}", trigger);

        if trigger == Some(".".to_string()) {
            // TODO: completion for struct members
            return Ok(None);
        }

        // Extract the partial word being typed from raw text
        let prefix = self.get_word_at_position(&src, pos);
        eprintln!("Completion prefix: '{}'", prefix);

        let mut suggestions: Vec<CompletionItem> = Vec::new();

        let (f, m) = {
            let b = self.builtins_doc.lock().await;
            (b.functions.clone(), b.methods.clone())
        };

        for (k, scr_fn) in f.iter() {
            util::fmatch(prefix, k);
            if k.starts_with(prefix) {
                suggestions.push(Self::comp_item_for_builtin(&k, scr_fn));
            }
        }

        for (k, scr_md) in m.iter() {
            util::fmatch(prefix, k);
            if k.starts_with(prefix) {
                suggestions.push(Self::comp_item_for_builtin(&k, scr_md));
            }
        }

        if let Some(local_sym_defs) = self.sym_defs.lock().await.get(&uri) {
            for sym in local_sym_defs {
                if sym.kind == SymbolKind::FUNCTION {
                    if sym.name.starts_with(prefix) {
                        suggestions.push(CompletionItem {
                            label: sym.name.clone(),
                            kind: Some(CompletionItemKind::FUNCTION),
                            detail: sym.detail.clone(),
                            ..Default::default()
                        });
                    }
                }

                for var in sym.children.as_ref().unwrap() {
                    if var.kind == SymbolKind::VARIABLE {
                        if var.name.starts_with(prefix) {
                            suggestions.push(CompletionItem {
                                label: var.name.clone(),
                                kind: Some(CompletionItemKind::VARIABLE),
                                detail: Some("Variable".to_string()),
                                ..Default::default()
                        });
                        }
                    }
                }
            }
        }

        Ok(Some(CompletionResponse::Array(suggestions)))
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
    eprintln!("we are here");

    let (service, socket) = LspService::new(|client| Backend::new(client));
    Server::new(stdin, stdout, socket).serve(service).await;
}
