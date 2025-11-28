//! Backend for our language server
use tower_lsp_server as tower_lsp;
use tower_lsp::Client;
use tower_lsp::lsp_types::*;
use tree_sitter::{Parser, Tree};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::Mutex;
use std::fmt::Debug;

use crate::brace::BracketChecker;
use crate::doc::Builtins;

pub struct Backend {
    /// The tower_lsp::Client instance
    pub client: Client,
    /// The BracketChecker instance
    ///
    /// TODO: Give this guy a better name, merge requests are welcome
    checker: Mutex<BracketChecker>,
    /// The Parser instance
    parser: Mutex<Parser>,
    /// Your workspace/project folder
    pub workspace_root: Mutex<Option<PathBuf>>,
    /// Stores parsed trees for each document
    pub trees: Mutex<HashMap<Uri, Tree>>,
    /// Function Definitions
    pub fn_defs: Mutex<HashMap<Uri, HashMap<String, (Range, Option<String>)>>>,
    /// Symbols
    pub sym_defs: Mutex<HashMap<Uri, Vec<DocumentSymbol>>>,
    /// For storing text content of scripts
    pub docs_content: Mutex<HashMap<Uri, String>>,
    /// Builtin functions and methods
    pub builtins_doc: Mutex<Builtins>,
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

        Self {
            client,
            checker: Mutex::new(BracketChecker::new()),
            parser: Mutex::new(parser),
            workspace_root: Mutex::new(None),
            trees: Mutex::new(HashMap::new()),
            fn_defs: Mutex::new(HashMap::new()),
            sym_defs: Mutex::new(HashMap::new()),
            docs_content: Mutex::new(HashMap::new()),
            builtins_doc: Mutex::new(Builtins::new()),
        }
    }
}

mod helpers;
mod navigation;
mod parsing;
