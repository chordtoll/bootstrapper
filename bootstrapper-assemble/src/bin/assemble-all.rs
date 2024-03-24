use std::{collections::BTreeMap, path::PathBuf};

use bootstrapper_assemble::{args::Args, assemble::build_recipe};
use bootstrapper_common::helpers::gen_graph_all;
use clap::Parser;
use petgraph::algo::toposort;

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let mut built = BTreeMap::new();

    for cache in ["build-cache/source", "build-cache/out", "build-cache/link"] {
        let pb = PathBuf::from(cache);
        if !pb.exists() {
            std::fs::create_dir_all(pb).unwrap();
        }
    }
    let g = gen_graph_all();
    let order: Vec<_> = toposort(&g, None)
        .unwrap_or_else(|x| panic!("Cycle detected: {}:{}",g[x.node_id()].name,g[x.node_id()].version))
        .iter()
        .map(|x| g.node_weight(*x).unwrap())
        .rev()
        .collect();

    let additional_salt = if args.no_checksum { "NOCHECKSUM" } else { "" };

    for i in order {
        build_recipe(
            &args,
            &i.name,
            &i.version,
            &mut built,
            false,
            additional_salt,
        )
        .await;
    }
}
