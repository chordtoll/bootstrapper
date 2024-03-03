use std::{collections::BTreeMap, path::PathBuf};

use bootstrapper_assemble::assemble::build_recipe;
use bootstrapper_common::helpers::gen_graph_all;
use petgraph::algo::toposort;

#[tokio::main]
async fn main() {
    let mut built = BTreeMap::new();

    for cache in ["build-cache/source", "build-cache/out", "build-cache/link"] {
        let pb = PathBuf::from(cache);
        if !pb.exists() {
            std::fs::create_dir_all(pb).unwrap();
        }
    }
    let g = gen_graph_all();
    let order: Vec<_> = toposort(&g, None)
        .unwrap()
        .iter()
        .map(|x| g.node_weight(*x).unwrap())
        .rev()
        .collect();
    for i in order {
        build_recipe(&i.name, &i.version, &mut built, false).await;
    }
}