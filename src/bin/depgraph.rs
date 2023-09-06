#![allow(clippy::iter_nth_zero)]

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use bootstrapper::{helpers::list_recipes, Recipe};

fn build_depgraph(
    target: &str,
    version: &str,
    built: &mut BTreeMap<(String, String), String>,
    depset: &mut BTreeMap<String, BTreeSet<String>>,
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
        let tag = build_depgraph(shell_img, shell_ver, built, depset);
        depset.entry(tag_dist.to_owned()).or_default().insert(tag);
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
            let tag = build_depgraph(dep_img, dep_ver, built, depset);
            depset.entry(tag_dist.to_owned()).or_default().insert(tag);
        }
    }

    tag_dist.to_owned()
}

fn main() {
    let mut built = BTreeMap::new();

    let mut deps = BTreeMap::new();

    let recipes = list_recipes();

    let mut recipes_by_section: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for (name, version) in recipes {
        let tag = build_depgraph(&name, &version, &mut built, &mut deps);
        recipes_by_section
            .entry(name.rsplit_once("/").unwrap().0.to_owned())
            .or_default()
            .insert(tag);
    }

    let mut transdeps = deps.clone();
    let mut tdl = transdeps.iter().map(|(_, x)| x.len()).sum::<usize>();
    loop {
        let keys: Vec<String> = transdeps.keys().cloned().collect();
        for k in keys {
            let v = transdeps[&k].clone();
            for d in v.iter() {
                if transdeps.contains_key(d) {
                    for dd in transdeps[d].clone() {
                        //println!("{} -> {} -> {}",k,d,dd);
                        transdeps.get_mut(&k).unwrap().insert(dd.to_owned());
                    }
                }
            }
        }
        let tdln = transdeps.iter().map(|(_, x)| x.len()).sum::<usize>();
        if tdln == tdl {
            break;
        }
        tdl = tdln;
    }

    println!("digraph graphname {{");
    for (s, rs) in recipes_by_section {
        println!("subgraph \"cluster_{}\" {{", s);
        println!("label=\"{}\"", s);
        for r in rs {
            println!("\"{}\";", r);
        }
        println!("color=blue");
        println!("}}");
    }
    for (k, v) in &transdeps {
        for d in v {
            let mut is_trans = false;
            for dd in v {
                if transdeps.contains_key(dd) {
                    if transdeps[dd].contains(d) {
                        is_trans = true;
                    }
                }
            }
            if !is_trans {
                println!("\"{}\" -> \"{}\";", k, d);
            }
        }
    }
    println!("}}");
}
