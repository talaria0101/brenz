//! Backend for our language server
use tower_lsp_server as tower_lsp;
use tower_lsp::Client;
use tower_lsp::lsp_types::*;
use tree_sitter::{Parser, Tree};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::Instant;
use std::fmt::Debug;

use crate::brace::BracketChecker;
use crate::doc::Builtins;

pub struct Backend {
    /// The tower_lsp::Client instance
    pub client: Client,
    /// The BracketChecker instance
    ///
    /// TODO: Give this guy a better name, merge requests are welcome
    pub checker: Arc<Mutex<BracketChecker>>,
    /// The Parser instance
    pub parser: Arc<Mutex<Parser>>,
    /// Last edit time
    pub last_edit_time: Arc<Mutex<HashMap<Uri, Instant>>>,
    /// Your workspace/project folder
    pub workspace_root: Arc<Mutex<Option<PathBuf>>>,
    /// Stores parsed trees for each document
    pub trees: Arc<Mutex<HashMap<Uri, Tree>>>,
    /// Function Definitions
    pub fn_defs: Arc<Mutex<HashMap<Uri, HashMap<String, (Range, Option<String>)>>>>,
    /// Symbols
    pub sym_defs: Arc<Mutex<HashMap<Uri, Vec<DocumentSymbol>>>>,
    /// For storing text content of scripts
    pub docs_content: Arc<Mutex<HashMap<Uri, String>>>,
    /// Builtin functions and methods
    pub builtins_doc: Arc<Mutex<Builtins>>,
}

impl Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend")
        .field("client", &self.client)
        .field("trees", &self.trees)
        .finish()
    }
}

impl Backend {
    pub fn new(client: Client) -> Self
    {
        let language = tree_sitter_gsc::LANGUAGE.into();
        let mut parser = Parser::new();
        parser.set_language(&language).expect("Error loading GSC language");

        let backend = Self {
            client,
            checker: Arc::new(Mutex::new(BracketChecker::new())),
            parser: Arc::new(Mutex::new(parser)),
            last_edit_time: Arc::new(Mutex::new(HashMap::new())),
            workspace_root: Arc::new(Mutex::new(None)),
            trees: Arc::new(Mutex::new(HashMap::new())),
            fn_defs: Arc::new(Mutex::new(HashMap::new())),
            sym_defs: Arc::new(Mutex::new(HashMap::new())),
            docs_content: Arc::new(Mutex::new(HashMap::new())),
            builtins_doc: Arc::new(Mutex::new(Builtins::new())),
        };
/*
        let backend_ref = Arc::new(backend);
        let backend_clone = backend_ref.clone();

        tokio::spawn(async move {
            loop {
                if parse_rx.changed().await.is_err() {
                    break;
                }

                let val = parse_rx.borrow().clone();
                let Some((uri, text)) = val else { continue };

                // Debounce: wait 300ms, if another change came in during
                // that time, the watch channel will have a new value
                sleep(Duration::from_millis(300)).await;

                let latest = parse_rx.borrow().clone();
                match latest {
                    Some((latest_uri, latest_text)) if latest_uri == uri && latest_text == text => {
                        backend_clone.parse_and_diagnose(uri, &text);
                    }
                    _ => continue
                }
            }
        });
*/
        backend
    }
}

mod helpers;
mod navigation;
mod parsing;
