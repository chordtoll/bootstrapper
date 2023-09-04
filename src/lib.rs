use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Source {
    pub url: String,
    pub sha: String,
    pub extract: Option<String>,
    pub noextract: Option<String>,
    pub copy: Option<Vec<String>>,
    pub chmod: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecipeBuildSteps {
    pub prepare: Option<Vec<String>>,
    pub configure: Option<Vec<String>>,
    pub compile: Option<Vec<String>>,
    pub install: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RecipeVersion {
    pub source: Option<Vec<Source>>,
    pub shell: Option<String>,
    pub deps: Option<Vec<String>>,
    pub mkdirs: Option<Vec<String>>,
    pub build: RecipeBuildSteps,
    pub artefacts: Vec<String>,
}

pub type Recipe = BTreeMap<String, RecipeVersion>;

pub mod helpers;