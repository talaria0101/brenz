mod util;
use util::log_print;
use tower_lsp::lsp_types::*;
use tower_lsp::{LanguageServer, LspService, Server, jsonrpc};
use async_trait::async_trait;

mod backend;
use backend::Backend;

mod brace;
mod interpreter;

#[async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult, jsonrpc::Error>
    {
        log_print("init brenz");
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
        log_print("Server initialized");
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

    async fn did_save(&self, params: DidSaveTextDocumentParams)
    {
        log_print(&format!("Saved: {}", params.text_document.uri.as_str()));
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams)
    {
        self.trees.lock().await.remove(&params.text_document.uri);
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

        let identifier = self.find_identifier_at_pos(tree, src, pos).unwrap();

        let fn_defs = self.fn_defs.lock().await;
        if let Some((def_uri, range)) = fn_defs.get(&identifier) {
            return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                uri: def_uri.clone(),
                range: *range,
            })));
        }

        Ok(None)
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| Backend::new(client));
    Server::new(stdin, stdout, socket).serve(service).await;
}
