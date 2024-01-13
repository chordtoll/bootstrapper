use std::{fs::File, io::Write, collections::BTreeSet, path::PathBuf};


use crate::Recipe;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Package {
    pub name: String,
    pub version: String,
}

impl From<String> for Package {
    fn from(value: String) -> Self {
        let name = value.split(':').nth(0).unwrap().to_owned();
        let version = value.split(':').nth(1).unwrap_or_else(|| {
            panic!(
                "Failed to find version in dep {}",value
            )
        }).to_owned();
        Self { name, version }
    }
}

impl From<&Package> for String {
    fn from(value: &Package) -> Self {
        format!("{}:{}",value.name,value.version)
    }
}

pub fn emit_run(f: &mut File, cmd: Vec<String>, shell: bool) {
    let cmd = if shell {
        format!("RUN {}", cmd.join(" "))
    } else {
        format!("RUN [\"{}\"]", cmd.join("\",\""))
    };
    f.write_all(cmd.as_bytes()).unwrap();
    f.write_all(b" \n").unwrap();
}

#[cfg(feature="assemble")]
pub async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    use futures_util::stream::StreamExt;
    // Reqwest setup
    let res = client
        .get(url)
        .send()
        .await
        .or(Err(format!("Failed to GET from '{}'", &url)))?;

    // Indicatif setup
    let (pb, ts) = if let Some(total_size) = res.content_length() {
        (indicatif::ProgressBar::new(total_size), total_size)
    } else {
        (indicatif::ProgressBar::new_spinner(), u64::MAX)
    };
    pb.set_style(indicatif::ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({bytes_per_sec}, {eta})").unwrap()
        .progress_chars("#>-"));

    // download chunks
    let mut downloaded: u64 = 0;
    let mut stream = res.bytes_stream();

    let mut res = Vec::new();

    while let Some(item) = stream.next().await {
        let chunk = item.or(Err("Error while downloading file"))?;
        let mut cvec = chunk.to_vec();
        res.append(&mut cvec);
        let new = std::cmp::min(downloaded + (chunk.len() as u64), ts);
        downloaded = new;
        pb.set_position(new);
    }

    pb.finish();
    Ok(res)
}

#[cfg(feature="assemble")]
pub fn envify(cmd: &str, env: &indexmap::IndexMap<String, String>) -> String {
    let mut cmd = cmd.to_owned();
    loop {
        let cmd_orig = cmd.clone();
        for (k, v) in env.iter() {
            cmd = cmd.replace(&format!("${{{}}}", k), v);
        }
        if cmd == cmd_orig {
            break;
        }
    }
    cmd
}

pub fn docker_export(tag: &String, path: String) {
    assert!(std::process::Command::new("docker")
        .args(["create", "--name", "ex", tag, "sh"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(std::process::Command::new("docker")
        .args(["export", "ex", "-o", &path])
        .output()
        .unwrap()
        .status
        .success());
    assert!(std::process::Command::new("docker")
        .args(["rm", "ex"])
        .output()
        .unwrap()
        .status
        .success());
}

#[cfg(any(feature="depgraph",feature="deplist"))]
pub fn list_recipes() -> BTreeSet<Package> {
    let mut res = BTreeSet::new();
    for entry in glob::glob("recipes/**/*.yaml").expect("Failed to read glob pattern") {
        let entry = entry.unwrap();
        let name = entry.strip_prefix("recipes").unwrap().as_os_str().to_str().unwrap().trim_end_matches(".yaml");
        let recipe: Recipe = serde_yaml::from_reader(std::fs::File::open(entry.clone()).unwrap()).unwrap();
        for version in recipe.keys() {
            res.insert(Package{name: name.to_string(), version: version.to_string()});
        }
    }
    res
}

pub fn get_deps(package: &Package) -> BTreeSet<Package> {
    let mut res = BTreeSet::new();

    let recipe_path = PathBuf::from(format!("recipes/{}.yaml", package.name));

    let mut recipe: Recipe = serde_yaml::from_reader(std::fs::File::open(recipe_path.clone()).unwrap()).unwrap();

    let recipe = recipe.remove(&package.version).unwrap();

    if let Some(deps) = recipe.deps {
        for i in deps {
            res.insert(i.into());
        }
    }

    res
}

#[cfg(feature="deplist")]
pub fn gen_graph_all() -> petgraph::graph::DiGraph<Package,()>
{
    let mut g = petgraph::graph::DiGraph::new();
    let mut indices = std::collections::BTreeMap::new();

    for package in list_recipes() {
        let idx=g.add_node(package.clone());
        indices.insert(package,idx);
    }
    for package in list_recipes() {
        for dep in get_deps(&package) {
            g.add_edge(*indices.get(&package).unwrap(),*indices.get(&dep).unwrap(),());
        }
    }
    g
}
