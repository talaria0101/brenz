//! Documentation for various functions and keywords
static BUILTINS: &str = include_str!("../assets/builtins.json");

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

use crate::backend::Backend;
use crate::util;

pub(crate) mod strings;

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ScrFunction {
    sign: String,
    info: String,
    params: Vec<ScrParam>,
    returns: Option<GscType>,
    example: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ScrParam {
    name: String,
    #[serde(rename = "type")]
    ptype: GscType,
    info: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum GscType {
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
    pub functions: HashMap<String, ScrFunction>,
}

impl Builtins {
    pub(crate) fn new() -> Self
    {
        Self { functions: HashMap::new() }
    }
}

impl Backend {
    pub(crate) fn unpack_docs(&self)
    {
        let data_dir = util::get_data_dir().unwrap();
        let docs_dir = data_dir.join("docs");
        let fns_file = docs_dir.join("builtins.json");
        if fns_file.exists() {
            return;
        }
        fs::create_dir_all(&docs_dir).unwrap();
        fs::write(fns_file, BUILTINS).expect("Couldn't write builtin functions to file");
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
}
