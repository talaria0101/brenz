mod util;
use util::log_print;

use tower_lsp::lsp_types::*;
use tower_lsp::{LanguageServer, LspService, Server, Client};
use async_trait::async_trait;
use tree_sitter::{Language, Parser, Tree, Node};

#[derive(Debug)]
struct Backend
{
    client: Client,
}

#[async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult, tower_lsp::jsonrpc::Error> {
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

    async fn initialized(&self, _: InitializedParams) {
        log_print("Server initialized");
        self.client.log_message(MessageType::INFO, "Brenz: Server Initialized").await;
    }

    async fn shutdown(&self) -> Result<(), tower_lsp::jsonrpc::Error> {
        self.client.log_message(MessageType::INFO, "Brenz: Server Shutdown").await;
        Ok(())
    }

    // Implement more methods like hover, diagnostics, etc.
    async fn did_open(&self, params: DidOpenTextDocumentParams)
    {
        self.client.log_message(MessageType::INFO, format!("{:?}", params)).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams)
    {
        self.client.log_message(MessageType::INFO, format!("{:?}", params)).await;
    }
}

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(|client| Backend {client});
    Server::new(stdin, stdout, socket).serve(service).await;
}
