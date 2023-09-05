use std::{fs::File, io::Write, cmp::min, collections::BTreeSet};

use futures_util::StreamExt;
use indexmap::IndexMap;
use indicatif::{ProgressStyle, ProgressBar};

use crate::Recipe;

pub fn emit_run(f: &mut File, cmd: Vec<String>, shell: bool) {
    let cmd = if shell {
        format!("RUN {}", cmd.join(" "))
    } else {
        format!("RUN [\"{}\"]", cmd.join("\",\""))
    };
    f.write_all(cmd.as_bytes()).unwrap();
    f.write_all(b" \n").unwrap();
}

pub async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    // Reqwest setup
    let res = client
        .get(url)
        .send()
        .await
        .or(Err(format!("Failed to GET from '{}'", &url)))?;

    // Indicatif setup
    let (pb, ts) = if let Some(total_size) = res.content_length() {
        (ProgressBar::new(total_size), total_size)
    } else {
        (ProgressBar::new_spinner(), u64::MAX)
    };
    pb.set_style(ProgressStyle::default_bar()
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
        let new = min(downloaded + (chunk.len() as u64), ts);
        downloaded = new;
        pb.set_position(new);
    }

    pb.finish();
    Ok(res)
}

pub fn envify(cmd: &str, env: &IndexMap<String, String>) -> String {
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

pub fn list_recipes() -> BTreeSet<(String,String)>{
    let mut res = BTreeSet::new();
    for entry in glob::glob("recipes/**/*.yaml").expect("Failed to read glob pattern") {
        let entry = entry.unwrap();
        let name = entry.strip_prefix("recipes").unwrap().as_os_str().to_str().unwrap().trim_end_matches(".yaml");
        let recipe: Recipe = serde_yaml::from_reader(std::fs::File::open(entry.clone()).unwrap()).unwrap();
        for version in recipe.keys() {
            res.insert((name.to_string(),version.to_string()));
        }
    }
    res
}