use std::{collections::BTreeMap, path::PathBuf};

use bootstrapper_assemble::args::Args;
use bootstrapper_assemble::assemble::build_recipe;
use clap::Parser;

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let target_name = args.target.split(':').nth(0).unwrap();
    let target_version = args.target.split(':').nth(1).unwrap();
    let mut built = BTreeMap::new();

    for cache in ["build-cache/source", "build-cache/out", "build-cache/link"] {
        let pb = PathBuf::from(cache);
        if !pb.exists() {
            std::fs::create_dir_all(pb).unwrap();
        }
    }

    build_recipe(target_name, target_version, &mut built, true).await;
}
