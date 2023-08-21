#![allow(clippy::iter_nth_zero)]

use std::{collections::BTreeMap, path::PathBuf};

use bootstrapper::Recipe;

fn build_depgraph(
    target: &str,
    version: &str,
    built: &mut BTreeMap<(String, String), String>,
) -> String {
    // Don't retry builds we've already done
    if let Some(tag) = built.get(&(target.to_owned(), version.to_owned())) {
        return tag.to_owned();
    }

    // Set up our paths
    let recipe_path = PathBuf::from(format!("recipes/{}.yaml", target));

    // Load recipe
    let mut recipe: Recipe =
        serde_yaml::from_reader(std::fs::File::open(recipe_path.clone()).unwrap()).unwrap();

    let recipe = recipe.remove(version).unwrap();

    let tag_dist = &format!("bootstrapper/{}:{}", target.replace('/', "."), version);

    built.insert((target.to_owned(), version.to_owned()), tag_dist.to_owned());

    // Load up a shell
    if let Some(shell) = &recipe.shell {
        let mut shell_it = shell.split(':');
        let shell_img = shell_it.next().unwrap();
        let shell_ver = shell_it.next().unwrap();
        let tag = build_depgraph(shell_img, shell_ver, built);
        println!("\"{}\" -> \"{}\";", tag_dist, tag);
    }

    // Copy in our sources
    //if let Some(source) = &recipe.source {
    //for s in source {
    //println!("\"{}\" -> \"bootstrapper/source:{}\";",tag_dist,s.sha);
    //}
    //}

    // Build and copy in our dependencies
    if let Some(deps) = &recipe.deps {
        for dep in deps {
            let dep_img = dep.split(':').nth(0).unwrap();
            let dep_ver = dep.split(':').nth(1).unwrap_or_else(|| {
                panic!(
                    "Failed to find version in dep {} while processing {}:{}",
                    dep, target, version
                )
            });
            let tag = build_depgraph(dep_img, dep_ver, built);
            println!("\"{}\" -> \"{}\";", tag_dist, tag);
        }
    }

    tag_dist.to_owned()
}

fn main() {
    let target = std::env::args().nth(1).unwrap();
    let target_name = target.split(':').nth(0).unwrap();
    let target_version = target.split(':').nth(1).unwrap();
    let mut built = BTreeMap::new();
    println!("digraph graphname {{");
    build_depgraph(target_name, target_version, &mut built);
    println!("}}");
}
