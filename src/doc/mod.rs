//! Documentation for various functions and keywords
static BUILTINS: &str = include_str!("../assets/builtins.ron");

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use tower_lsp_server::lsp_types::Hover;

use crate::backend::Backend;
use crate::util;

pub(crate) mod serde_helper;
pub(crate) mod strings;
use serde_helper::deserialize_lower_map;

/// A builtin script function or method
pub(crate) trait ScriptCallable {
    fn kind(&self) -> String;
    fn camel_name(&self) -> Option<String>;
    fn sign(&self) -> String;
    fn info(&self) -> String;
    fn called_on(&self) -> String;
    fn param_names(&self) -> Vec<String>;
    fn example(&self) -> String;
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(crate) struct ScrFunction {
    pub camel_name: Option<String>,
    pub sign: String,
    pub info: String,
    pub params: Vec<ScrParam>,
    pub returns: Option<GscType>,
    pub example: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(crate) struct ScrMethod {
    pub camel_name: Option<String>,
    pub sign: String,
    pub info: String,
    pub called_on: String,
    pub params: Vec<ScrParam>,
    pub returns: Option<GscType>,
    pub example: Option<String>,
}

impl ScriptCallable for ScrFunction {
    fn kind(&self) -> String {
        "Builtin Function".to_string()
    }
    fn camel_name(&self) -> Option<String> {
        self.camel_name.clone()
    }
    fn sign(&self) -> String {
        self.sign.clone()
    }
    fn info(&self) -> String {
        self.info.clone()
    }
    fn called_on(&self) -> String {
        "".to_string()
    }
    fn param_names(&self) -> Vec<String> {
        self.params.iter().map(|p| p.name.clone()).collect()
    }
    fn example(&self) -> String {
        if let Some(ref ex) = self.example {
            ex.clone()
        } else {
            "No example, consider contributing!".to_string()
        }
    }
}
impl ScriptCallable for ScrMethod {
    fn kind(&self) -> String {
        "Builtin Method".to_string()
    }
    fn camel_name(&self) -> Option<String> {
        self.camel_name.clone()
    }
    fn sign(&self) -> String {
        self.sign.clone()
    }
    fn info(&self) -> String {
        self.info.clone()
    }
    fn called_on(&self) -> String {
        self.called_on.clone()
    }
    fn param_names(&self) -> Vec<String> {
        self.params.iter().map(|p| p.name.clone()).collect()
    }
    fn example(&self) -> String {
        if let Some(ref ex) = self.example {
            ex.clone()
        } else {
            "No example, consider contributing!".to_string()
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ScrParam {
    pub name: String,
    #[serde(rename = "type")]
    pub ptype: GscType,
    pub info: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum GscType {
    Any,
    Bool,
    Int,
    Float,
    String,
    LString,
    Array,
    Vector,
    Entity,
    HudElem,
    Struct,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct Builtins {
    #[serde(deserialize_with = "deserialize_lower_map")]
    pub functions: HashMap<String, ScrFunction>,
    #[serde(deserialize_with = "deserialize_lower_map")]
    pub methods: HashMap<String, ScrMethod>,
}

impl Builtins {
    /// Empty docs: what the server holds before `load_docs` fills it.
    pub(crate) fn new() -> Self {
        Self {
            functions: HashMap::new(),
            methods: HashMap::new(),
        }
    }
}

impl Backend {
    /// Unpack the embedded docs where the server expects them.
    /// Always overwrites: the copy belongs to the binary version.
    pub(crate) fn unpack_docs(&self) {
        let data_dir = util::get_data_dir().unwrap();
        let docs_dir = data_dir.join("docs");
        let fns_file = docs_dir.join("builtins.ron");
        if !docs_dir.exists() {
            fs::create_dir_all(&docs_dir).unwrap();
        }
        // Always refresh: the file is unpacked from the binary, so a
        // stale copy from an older Brenz would otherwise linger.
        fs::write(&fns_file, BUILTINS).expect("Couldn't write builtin functions to file");
        // Drop the artifacts of the old JSON format, if present.
        let _ = fs::remove_file(docs_dir.join("builtins.json"));
        let _ = fs::remove_file(docs_dir.join("brenz-builtins-schema.json"));
    }

    /// Load the unpacked docs into memory, falling back to the
    /// embedded copy when the file went missing or broke.
    pub(crate) async fn load_docs(&self) {
        let data_dir = util::get_data_dir().unwrap();
        let docs_dir = data_dir.join("docs");
        let fns_file = docs_dir.join("builtins.ron");

        let content = tokio::fs::read_to_string(&fns_file)
            .await
            .unwrap_or_else(|_| BUILTINS.to_string());
        let builtins: Builtins = ron::from_str(&content).unwrap_or_else(|e| {
            util::logprint!(
                util::LogType::Error,
                "Couldn't parse builtin functions file, using embedded docs: {e}"
            );
            ron::from_str(BUILTINS).expect("embedded builtins.ron")
        });

        *self.builtins_doc.lock().await = builtins;
    }

    /// Tiny wrapper turning info text into a hover response.
    fn identifier_hover_info_get_hover(&self, info: String) -> Option<Hover> {
        use crate::{HoverContents, MarkedString};
        Some(Hover {
            contents: HoverContents::Scalar(MarkedString::String(info)),
            range: None,
        })
    }

    /// One-liners for the magic words (`self`, `level`, `wait`
    /// ...). Everything else resolves through the builtin tables.
    pub(crate) fn identifier_hover_info(&self, identifier: &str) -> Option<Hover> {
        let txt = match identifier {
            "wait" => strings::WAIT_INFO.to_string(),
            "thread" => strings::THREAD_INFO.to_string(),
            "self" => strings::SELF_INFO.to_string(),
            "level" => strings::LEVEL_INFO.to_string(),
            "game" => strings::GAME_INFO.to_string(),
            _ => String::new(),
        };
        if txt.is_empty() {
            return None;
        }

        self.identifier_hover_info_get_hover(txt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_ron_parses_and_covers_coduomp() {
        let b: Builtins = ron::from_str(BUILTINS).expect("builtins.ron must parse");
        // coduomp: 120 script functions, 172 script methods (plus aliases).
        assert!(b.functions.len() >= 120, "functions: {}", b.functions.len());
        assert!(b.methods.len() >= 172, "methods: {}", b.methods.len());
        for key in [
            "spawn",
            "getent",
            "isdefined",
            "objective_add",
            "precachemodel",
        ] {
            assert!(b.functions.contains_key(key), "missing function {key}");
        }
        for key in ["settext", "dodamage", "getstance", "fireturret", "suicide"] {
            assert!(b.methods.contains_key(key), "missing method {key}");
        }
    }
}
