use clap::Parser;
use std::path::PathBuf;
use tokio::time::{Duration, Instant};
use tower_lsp::lsp_types::*;
use tower_lsp::{LanguageServer, LspService, Server, jsonrpc};
use tower_lsp_server as tower_lsp;

mod backend;
use backend::Backend;
mod brace;
mod compiler;
mod config;
mod util;
use util::{LogType, logprint};
mod doc;
mod pk3;

impl LanguageServer for Backend {
    async fn initialize(
        &self,
        params: InitializeParams,
    ) -> Result<InitializeResult, jsonrpc::Error> {
        self.unpack_docs();
        self.load_docs().await;
        let (f, m) = {
            let b = self.builtins_doc.lock().await;
            (b.functions.clone(), b.methods.clone())
        };
        logprint!(
            LogType::Info,
            "Builtin functions: \n{}",
            ron::ser::to_string_pretty(&f, util::ron_pcfg().to_owned()).unwrap()
        );
        logprint!(
            LogType::Info,
            "Builtin methods: \n{}",
            ron::ser::to_string_pretty(&m, util::ron_pcfg().to_owned()).unwrap()
        );
        // Thanks to Claude for helping with workspace root
        self.client
            .log_message(
                MessageType::LOG,
                format!(
                    "Workspace folders: {:#?}",
                    params.workspace_folders.as_ref()
                ),
            )
            .await;
        let workspace_root = params
            .workspace_folders
            .as_ref()
            .and_then(|folders| folders.first())
            .map(|folder| PathBuf::from(folder.uri.path().as_str()))
            .or_else(|| {
                #[allow(deprecated)]
                params.root_uri.map(|u| PathBuf::from(u.path().as_str()))
            });

        if let Some(root) = &workspace_root {
            self.client
                .log_message(MessageType::INFO, format!("Workspace root: {:?}", root))
                .await;
        }

        *self.workspace_root.lock().await = workspace_root;

        // Remember snippet support: completions render `${1:param}`
        // tab stops only for clients that understand them.
        let snippet = params
            .capabilities
            .text_document
            .and_then(|td| td.completion)
            .and_then(|c| c.completion_item)
            .and_then(|i| i.snippet_support)
            .unwrap_or(false);
        *self.snippet_support.lock().await = snippet;

        // Load the `.brenz` project config, then (re)build the `.pk3`
        // index for the configured game paths. Only the central
        // directory of each archive is read, so this stays fast.
        let root = self.workspace_root.lock().await.clone();
        let cfg = crate::config::BrenzConfig::load(root.as_ref());
        if !cfg.pk3_paths.is_empty() {
            self.client
                .log_message(
                    MessageType::INFO,
                    format!("Brenz: pk3 paths: {:?}", cfg.pk3_paths),
                )
                .await;
        }
        let index = crate::pk3::Pk3Index::load_or_build(root.as_ref(), &cfg.pk3_paths);
        *self.config.lock().await = cfg;
        *self.pk3_index.lock().await = index;

        logprint!(LogType::Info, "Brenz initializing");
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                document_symbol_provider: Some(OneOf::Left(true)),
                hover_provider: Some(true.into()),
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                definition_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
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

    // Handshake done: log where we landed so the operator can see it.
    async fn initialized(&self, _: InitializedParams) {
        logprint!(LogType::Success, "Brenz initialized");
        self.client
            .log_message(MessageType::INFO, "Brenz: Server Initialized")
            .await;
        logprint!(
            LogType::Info,
            "Workspace: {:?}",
            self.workspace_root.lock().await
        );
    }

    // Politely acknowledge shutdown. Nothing to clean up.
    async fn shutdown(&self) -> Result<(), tower_lsp::jsonrpc::Error> {
        self.client
            .log_message(MessageType::INFO, "Brenz: Server Shutdown")
            .await;
        Ok(())
    }

    // A document opened: stash its text, then parse and diagnose.
    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        self.client
            .log_message(
                MessageType::INFO,
                format!("Opened: {}", params.text_document.uri.as_str()),
            )
            .await;

        // Parse the newly opened document
        self.docs_content.lock().await.insert(
            params.text_document.uri.clone(),
            params.text_document.text.clone(),
        );
        self.parse_and_diagnose(params.text_document.uri.clone(), &params.text_document.text)
            .await;
    }

    // Full-sync edits with a 300ms debounce, so typing never
    // re-parses on every keystroke.
    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // For full sync, we get the entire document content
        if let Some(change) = params.content_changes.first() {
            self.client
                .log_message(
                    MessageType::INFO,
                    format!("Changed: {}", params.text_document.uri.as_str()),
                )
                .await;

            let uri = params.text_document.uri.clone();
            let text = change.text.clone();
            let now = Instant::now();

            self.docs_content
                .lock()
                .await
                .insert(params.text_document.uri, change.text.clone());
            self.last_edit_time.lock().await.insert(uri.clone(), now);

            let this = self.clone();
            let last_edit = self.last_edit_time.clone();

            tokio::spawn(async move {
                // wait 300 ms
                tokio::time::sleep(Duration::from_millis(300)).await;

                let last = last_edit.lock().await.get(&uri).copied();
                if last == Some(now) {
                    this.parse_and_diagnose(uri.clone(), &text).await;
                }
            });
        }
    }

    // Saves need no work: opens and edits already keep everything current.
    async fn did_save(&self, _params: DidSaveTextDocumentParams) {
        //log_print(&format!("Saved: {}", params.text_document.uri.as_str()));
    }

    // Forget the closed document: tree, text, debounce state, and
    // its diagnostics. Definitions stay, other files may need them.
    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.trees.lock().await.remove(&params.text_document.uri);
        self.docs_content
            .lock()
            .await
            .remove(&params.text_document.uri);
        self.last_edit_time
            .lock()
            .await
            .remove(&params.text_document.uri);
        self.client
            .publish_diagnostics(params.text_document.uri, vec![], None)
            .await;
        // not clearing the fn_defs for this file
    }

    // Jump to a function definition or a variable's latest write.
    // Only identifiers participate; anything else bows out.
    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
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

        if let Some(fn_node) = self
            .find_parent_of_kind(node, "direct_call")
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
                uri,
                range,
            })));
        }

        Ok(None)
    }

    // Hover cards: keywords first, then builtins, then user
    // functions with their doc comments.
    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
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
                if let Some(info) =
                    self.identifier_hover_info(&src[node.start_byte()..node.end_byte()])
                {
                    return Ok(Some(info));
                }
            }
            _ => {}
        }

        // Only identifiers can go beyond this point
        if node.kind() != "identifier" {
            let strr = &src[node.start_byte()..node.end_byte()];
            self.client
                .log_message(MessageType::LOG, &format!("{}, {}", strr, node.kind()))
                .await;
            return Ok(None);
        }

        let identifier = &src[node.start_byte()..node.end_byte()];
        self.client
            .log_message(MessageType::LOG, &format!("identifier: {}", identifier))
            .await;

        if let Some(info) = self.identifier_hover_info(identifier) {
            return Ok(Some(info));
        }

        if let Some(call_node) = self
            .find_parent_of_kind(node, "direct_call")
            .or_else(|| self.find_parent_of_kind(node, "thread_call"))
            .or_else(|| self.find_parent_of_kind(node, "object_call"))
            .or_else(|| self.find_parent_of_kind(node, "function_pointer"))
        {
            logprint!(LogType::Info, "(hover) in a call_node");
            let builtin_info: String = {
                let r = self.builtins_doc.lock().await;
                logprint!(
                    LogType::Info,
                    "The identifier in question: {}",
                    &identifier.to_lowercase()
                );
                logprint!(
                    LogType::Info,
                    "Builtin functions: {:#?}",
                    r.functions.keys()
                );
                let b = r.methods.get(&identifier.to_ascii_lowercase());

                if let Some(b) = b {
                    Self::info_from_builtin(b, &b.sign)
                } else {
                    let b = r.functions.get(&identifier.to_ascii_lowercase());
                    if let Some(b) = b {
                        Self::info_from_builtin(b, &b.sign)
                    } else {
                        String::new()
                    }
                }
            };
            if !builtin_info.is_empty() {
                logprint!(LogType::Info, "txt: {}", &builtin_info);
                return Ok(Some(Hover {
                    contents: HoverContents::Scalar(MarkedString::String(builtin_info)),
                    range: None,
                }));
            }

            match self.resolve_target_fn(call_node, &src, &uri, true).await {
                Some(res) => {
                    let txt = self
                        .hover_info(res, uri)
                        .await
                        .unwrap_or(String::from("Failed to get hover info"));
                    return Ok(Some(Hover {
                        contents: HoverContents::Scalar(MarkedString::String(txt)),
                        range: None,
                    }));
                }
                None => {
                    logprint!(LogType::Error, "Failed to resolve_target_fn");
                }
            }
        }

        Ok(Some(Hover {
            contents: HoverContents::Scalar(MarkedString::String(
                "We're not there yet.\n[Contribute :)](https://gitlab.com/kazam0180/brenz)"
                    .to_string(),
            )),
            range: None,
        }))
    }

    // The file's symbol tree, straight from the cached parse.
    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> jsonrpc::Result<Option<DocumentSymbolResponse>> {
        let uri = params.text_document.uri;
        let symbols = match self.get_syms(&uri).await {
            Some(syms) => {
                //logprint!(LogType::Info, "Symobols: {:#?}", syms.clone());
                syms
            }
            None => {
                logprint!(
                    LogType::Error,
                    "Failed to get document symbols for file: {}",
                    uri.path().as_str()
                );
                Vec::new()
            }
        };

        Ok(Some(DocumentSymbolResponse::Nested(symbols)))
    }

    // Completions: builtins with call snippets, local functions the
    // same way, in-scope variables boosted by expected type, plus
    // keywords. Prefix and substring matches only; the client hides
    // the rest anyway.
    async fn completion(
        &self,
        params: CompletionParams,
    ) -> jsonrpc::Result<Option<CompletionResponse>> {
        use crate::backend::completion as comp;
        use crate::doc::GscType;

        let uri = params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;

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

        // Check if triggered by a character
        let trigger = params.context.and_then(|ctx| ctx.trigger_character);

        if trigger == Some(".".to_string()) {
            // TODO: completion for struct members
            return Ok(None);
        }

        // Extract the partial word being typed from raw text
        let prefix = self.get_word_at_position(&src, pos);
        let byte = Self::position_to_byte(&src, pos);
        let snippet = *self.snippet_support.lock().await;

        // When completing a call argument, the callee's declared
        // parameter type ranks same-type variables first.
        let expected: Option<GscType> = match comp::enclosing_call(&tree, &src, byte) {
            Some((name, index, is_method)) => {
                let docs = self.builtins_doc.lock().await;
                let params = if is_method {
                    docs.methods.get(&name).map(|m| m.params.clone())
                } else {
                    docs.functions.get(&name).map(|f| f.params.clone())
                };
                params
                    .and_then(|p| p.get(index).map(|param| param.ptype.clone()))
                    .filter(|ty| *ty != GscType::Any)
            }
            None => None,
        };

        let mut suggestions: Vec<CompletionItem> = Vec::new();
        // (tier, score desc, name): same-type variables first.

        let (f, m) = {
            let b = self.builtins_doc.lock().await;
            (b.functions.clone(), b.methods.clone())
        };

        for (k, scr_fn) in f.iter() {
            let score = comp::fuzzy_score(prefix, k);
            if score == 0 {
                continue;
            }
            let mut item = Self::comp_item_for_builtin(k, scr_fn, snippet);
            item.sort_text = Some(format!(
                "1-{:07}-{k}",
                comp::SCORE_SORT_MAX - score.min(comp::SCORE_SORT_MAX)
            ));
            suggestions.push(item);
        }

        for (k, scr_md) in m.iter() {
            let score = comp::fuzzy_score(prefix, k);
            if score == 0 {
                continue;
            }
            let mut item = Self::comp_item_for_builtin(k, scr_md, snippet);
            item.sort_text = Some(format!(
                "1-{:07}-{k}",
                comp::SCORE_SORT_MAX - score.min(comp::SCORE_SORT_MAX)
            ));
            suggestions.push(item);
        }

        if let Some(local_sym_defs) = self.sym_defs.lock().await.get(&uri) {
            for sym in local_sym_defs {
                if sym.kind == SymbolKind::FUNCTION && comp::fuzzy_score(prefix, &sym.name) > 0 {
                    let params = comp::head_params(sym.detail.as_deref().unwrap_or(""));
                    let (text, format) = comp::call_text(&sym.name, &params, snippet);
                    suggestions.push(CompletionItem {
                        label: sym.name.clone(),
                        kind: Some(CompletionItemKind::FUNCTION),
                        detail: sym.detail.clone(),
                        insert_text: Some(text),
                        insert_text_format: format,
                        sort_text: Some(format!(
                            "1-{:07}-{}",
                            comp::SCORE_SORT_MAX
                                - comp::fuzzy_score(prefix, &sym.name).min(comp::SCORE_SORT_MAX),
                            sym.name.to_lowercase()
                        )),
                        ..Default::default()
                    });
                }

                for var in sym.children.as_ref().unwrap() {
                    if var.kind == SymbolKind::VARIABLE {
                        let score = comp::fuzzy_score(prefix, &var.name);
                        if score == 0 {
                            continue;
                        }
                        // Same-type variables float to the top when the
                        // argument slot declares a type.
                        let var_ty = comp::scope_vars(&tree, &src, byte)
                            .into_iter()
                            .find(|v| v.name == var.name)
                            .and_then(|v| v.ty);
                        let tier = u32::from(
                            !(expected.is_some() && var_ty.is_some() && expected == var_ty),
                        );
                        let detail = match var_ty {
                            Some(t) => format!("Variable: {t:?}").to_lowercase(),
                            None => "Variable".to_string(),
                        };
                        suggestions.push(CompletionItem {
                            label: var.name.clone(),
                            kind: Some(CompletionItemKind::VARIABLE),
                            detail: Some(detail),
                            sort_text: Some(format!(
                                "{tier}-{:07}-{}",
                                comp::SCORE_SORT_MAX - score.min(comp::SCORE_SORT_MAX),
                                var.name.to_lowercase()
                            )),
                            ..Default::default()
                        });
                    }
                }
            }
        }

        for kw in [
            "if",
            "else",
            "while",
            "for",
            "return",
            "break",
            "continue",
            "switch",
            "case",
            "default",
            "wait",
            "thread",
            "true",
            "false",
            "undefined",
            "self",
        ] {
            if comp::fuzzy_score(prefix, kw) > 0 {
                suggestions.push(CompletionItem {
                    label: kw.to_string(),
                    kind: Some(CompletionItemKind::KEYWORD),
                    sort_text: Some(format!("2-{kw}")),
                    ..Default::default()
                });
            }
        }

        Ok(Some(CompletionResponse::Array(suggestions)))
    }

    // Quickfixes. Currently just the `foreach` rewrite; anything
    // else gets a polite nothing.
    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> jsonrpc::Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri.clone();
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

        if let Some((range, new_text)) = self.foreach_fix(&tree, &src, params.range.start) {
            let related: Vec<Diagnostic> = params
                .context
                .diagnostics
                .into_iter()
                .filter(|d| ranges_overlap(&d.range, &range))
                .collect();
            // `Uri` carries interior mutability, but it is only ever
            // used here as an immutable map key, so the lint does not
            // apply to how the key is actually used.
            #[allow(clippy::mutable_key_type)]
            let mut changes = std::collections::HashMap::new();
            changes.insert(uri, vec![TextEdit { range, new_text }]);
            return Ok(Some(vec![CodeActionOrCommand::CodeAction(CodeAction {
                title: "Convert foreach to for loop".to_string(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: Some(related),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                }),
                ..Default::default()
            })]));
        }

        Ok(None)
    }
}

fn ranges_overlap(a: &Range, b: &Range) -> bool {
    // Overlap test for attaching the right diagnostics to a fix:
    // touching at an edge still counts.
    let (a_start, a_end) = (
        (a.start.line, a.start.character),
        (a.end.line, a.end.character),
    );
    let (b_start, b_end) = (
        (b.start.line, b.start.character),
        (b.end.line, b.end.character),
    );
    a_start <= b_end && b_start <= a_end
}

#[derive(Parser)]
struct Args {
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
        unsafe {
            std::env::set_var("RUST_BACKTRACE", "1");
        }
    }
    if args.init {
        match util::init_cfg() {
            Ok(p) => {
                logprint!(
                    LogType::Success,
                    "Initialized a project in: {}",
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

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
