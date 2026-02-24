//! Documentation for various functions and keywords
static BUILTINS: &str = include_str!("../assets/builtins.json");
static BUILTINS_SCHEMA: &str = include_str!("../assets/brenz-builtins-schema.json");

use serde::{Deserialize, Serialize};
use tower_lsp_server::lsp_types::Hover;
use std::collections::HashMap;
use std::fs;

use crate::backend::Backend;
use crate::util;

pub(crate) mod strings;
pub(crate) mod serde_helper;
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
    pub example: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(crate) struct ScrMethod {
    pub camel_name: Option<String>,
    pub sign: String,
    pub info: String,
    pub called_on: String,
    pub params: Vec<ScrParam>,
    pub returns: Option<GscType>,
    pub example: String,
}

impl ScriptCallable for ScrFunction {
    fn kind(&self) -> String
    {
        "Builtin Function".to_string()
    }
    fn camel_name(&self) -> Option<String>
    {
        match &self.camel_name {
            Some(cn) => Some(cn.clone()),
            None => None
        }
    }
    fn sign(&self) -> String
    {
        self.sign.clone()
    }
    fn info(&self) -> String
    {
        self.info.clone()
    }
    fn called_on(&self) -> String
    {
        "".to_string()
    }
    fn param_names(&self) -> Vec<String>
    {
        self.params.iter().map(|p| p.name.clone()).collect()
    }
    fn example(&self) -> String
    {
        self.example.clone()
    }
}
impl ScriptCallable for ScrMethod {
    fn kind(&self) -> String
    {
        "Builtin Method".to_string()
    }
    fn camel_name(&self) -> Option<String>
    {
        match &self.camel_name {
            Some(cn) => Some(cn.clone()),
            None => None
        }
    }
    fn sign(&self) -> String
    {
        self.sign.clone()
    }
    fn info(&self) -> String
    {
        self.info.clone()
    }
    fn called_on(&self) -> String
    {
        self.called_on.clone()
    }
    fn param_names(&self) -> Vec<String>
    {
        self.params.iter().map(|p| p.name.clone()).collect()
    }
    fn example(&self) -> String
    {
        self.example.clone()
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub(crate) struct ScrParam {
    name: String,
    #[serde(rename = "type")]
    ptype: GscType,
    info: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "lowercase")]
pub(crate) enum GscType {
    Any,
    Bool,
    Int,
    Float,
    String,
    LString,
    Array,
    Vector,
    Entity,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Builtins {
    #[serde(deserialize_with = "deserialize_lower_map")]
    pub functions: HashMap<String, ScrFunction>,
    #[serde(deserialize_with = "deserialize_lower_map")]
    pub methods: HashMap<String, ScrMethod>,
}

impl Builtins {
    pub(crate) fn new() -> Self
    {
        Self { functions: HashMap::new(), methods: HashMap::new() }
    }
}

impl Backend {
    pub(crate) fn unpack_docs(&self)
    {
        let data_dir = util::get_data_dir().unwrap();
        let docs_dir = data_dir.join("docs");
        let fns_file = docs_dir.join("builtins.json");
        let sch_file = docs_dir.join("brenz-builtins-schema.json");
        if !docs_dir.exists() {
            fs::create_dir_all(&docs_dir).unwrap();
        }
        if !fns_file.exists() {
            fs::write(fns_file, BUILTINS).expect("Couldn't write builtin functions to file");
        }
        if !sch_file.exists() {
            fs::write(sch_file, BUILTINS_SCHEMA).expect("Couldn't write builtin schema to file");
        }
    }

    pub(crate) async fn load_docs(&self)
    {
        let data_dir = util::get_data_dir().unwrap();
        let docs_dir = data_dir.join("docs");
        let fns_file = docs_dir.join("builtins.json");

        let content = tokio::fs::read_to_string(fns_file).await
            .expect("Couldn't read builtin functions file");
        let builtins: Builtins = serde_json::from_str(&content)
            .expect("Couldn't parse builtin functions file");

        *self.builtins_doc.lock().await = builtins;
    }

    fn identifier_hover_info_get_hover(&self, info: String) -> Option<Hover>
    {
        use crate::{HoverContents, MarkedString};
        Some(Hover {
            contents: HoverContents::Scalar(MarkedString::String(info)),
            range: None
        })
    }

    pub(crate) fn identifier_hover_info(&self, identifier: &str) -> Option<Hover>
    {
        let txt = match identifier {
            "wait" => strings::WAIT_INFO.to_string(),
            "thread" => strings::THREAD_INFO.to_string(),
            "self" => strings::SELF_INFO.to_string(),
            "level" => strings::LEVEL_INFO.to_string(),
            "game" => strings::GAME_INFO.to_string(),
            _ => String::new()
        };
        if txt.is_empty() {
            return None;
        }

        self.identifier_hover_info_get_hover(txt)
    }
}
