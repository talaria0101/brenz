//! Backend for our language server
use std::collections::HashMap;
use std::fmt::Debug;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tower_lsp::Client;
use tower_lsp::lsp_types::*;
use tower_lsp_server as tower_lsp;
use tree_sitter::{Parser, Tree};

pub mod completion;

/// Function definitions per document: name to (range, doc comment).
pub(crate) type FnDefs = HashMap<String, (Range, Option<String>)>;

use crate::brace::BracketChecker;
use crate::config::BrenzConfig;
use crate::doc::Builtins;
use crate::include::IncludeIndex;

#[derive(Clone)]
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
    /// Whether the client renders snippet tab stops.
    pub snippet_support: Arc<Mutex<bool>>,
    /// Stores parsed trees for each document
    pub trees: Arc<Mutex<HashMap<Uri, Tree>>>,
    /// Function Definitions
    pub fn_defs: Arc<Mutex<HashMap<Uri, FnDefs>>>,
    /// Symbols
    pub sym_defs: Arc<Mutex<HashMap<Uri, Vec<DocumentSymbol>>>>,
    /// For storing text content of scripts
    pub docs_content: Arc<Mutex<HashMap<Uri, String>>>,
    /// Builtin functions and methods
    pub builtins_doc: Arc<Mutex<Builtins>>,
    /// Parsed `.brenz` project configuration
    pub config: Arc<Mutex<BrenzConfig>>,
    /// Index of scripts inside `.pk3` archives from `pk3_paths`
    pub include_index: Arc<Mutex<IncludeIndex>>,
    /// Serializes on-demand dependency loads (`load_many_scripts`).
    /// Without it two concurrent tasks (e.g. diagnostics and hover)
    /// both see a script as missing and both pay the full read plus
    /// parse plus extract, which on 400KB waypoint files is tens of
    /// seconds each. Only ever taken at the top of `load_many_scripts`,
    /// never while holding another lock in the other order, so it
    /// cannot deadlock: waiters simply find the script cached.
    pub dep_lock: Arc<Mutex<()>>,
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
    /// Fresh backend: parser loaded with the GSC grammar, everything
    /// else empty until documents open and `initialize` runs.
    pub fn new(client: Client) -> Self {
        let language = tree_sitter_gsc::LANGUAGE.into();
        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .expect("Error loading GSC language");

        let backend = Self {
            client,
            checker: Arc::new(Mutex::new(BracketChecker::new())),
            parser: Arc::new(Mutex::new(parser)),
            last_edit_time: Arc::new(Mutex::new(HashMap::new())),
            workspace_root: Arc::new(Mutex::new(None)),
            snippet_support: Arc::new(Mutex::new(false)),
            trees: Arc::new(Mutex::new(HashMap::new())),
            fn_defs: Arc::new(Mutex::new(HashMap::new())),
            sym_defs: Arc::new(Mutex::new(HashMap::new())),
            docs_content: Arc::new(Mutex::new(HashMap::new())),
            builtins_doc: Arc::new(Mutex::new(Builtins::new())),
            config: Arc::new(Mutex::new(BrenzConfig::default())),
            include_index: Arc::new(Mutex::new(IncludeIndex::default())),
            dep_lock: Arc::new(Mutex::new(())),
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

mod diagnostics;
mod helpers;
mod navigation;
mod parsing;
