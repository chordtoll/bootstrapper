use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    path::PathBuf,
};

use glob::glob;
use petgraph::graph::DiGraph;

use crate::recipe::Recipe;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Package {
    pub name: String,
    pub version: String,
}

impl From<String> for Package {
    fn from(value: String) -> Self {
        let name = value.split(':').nth(0).unwrap().to_owned();
        let version = value
            .split(':')
            .nth(1)
            .unwrap_or_else(|| panic!("Failed to find version in dep {}", value))
            .to_owned();
        Self { name, version }
    }
}

impl From<&Package> for String {
    fn from(value: &Package) -> Self {
        format!("{}:{}", value.name, value.version)
    }
}

pub fn list_recipes() -> BTreeSet<Package> {
    let mut res = BTreeSet::new();
    for entry in glob("recipes/**/*.yaml").expect("Failed to read glob pattern") {
        let entry = entry.unwrap();
        let name = entry
            .strip_prefix("recipes")
            .unwrap()
            .as_os_str()
            .to_str()
            .unwrap()
            .trim_end_matches(".yaml");
        let recipe: Recipe = serde_yaml::from_reader(File::open(entry.clone()).unwrap()).unwrap();
        for version in recipe.keys() {
            res.insert(Package {
                name: name.to_string(),
                version: version.to_string(),
            });
        }
    }
    res
}

pub fn get_deps(package: &Package) -> BTreeSet<Package> {
    let mut res = BTreeSet::new();

    let recipe_path = PathBuf::from(format!("recipes/{}.yaml", package.name));

    let mut recipe: Recipe =
        serde_yaml::from_reader(File::open(recipe_path.clone()).unwrap()).unwrap();

    let recipe = recipe.remove(&package.version).unwrap();

    if let Some(deps) = recipe.deps {
        for i in deps {
            res.insert(i.into());
        }
    }

    res
}

pub fn gen_graph_all() -> DiGraph<Package, ()> {
    let mut g = DiGraph::new();
    let mut indices = BTreeMap::new();

    for package in list_recipes() {
        let idx = g.add_node(package.clone());
        indices.insert(package, idx);
    }
    for package in list_recipes() {
        for dep in get_deps(&package) {
            g.add_edge(
                *indices.get(&package).unwrap(),
                *indices.get(&dep).unwrap(),
                (),
            );
        }
    }
    g
}
