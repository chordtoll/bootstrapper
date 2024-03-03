use std::{cmp::min, io::Write};

use bollard::{
    container::{Config, CreateContainerOptions},
    Docker,
};
use futures_util::{future::ready, StreamExt};
use indexmap::IndexMap;
use indicatif::{ProgressBar, ProgressStyle};

pub fn emit_run<W: Write>(f: &mut W, cmd: Vec<String>, shell: bool) {
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

pub fn alias(cmd: &str, alias: &IndexMap<String, String>) -> String {
    let mut cmd = cmd.to_owned();
    for (k, v) in alias {
        if let Some(c) = cmd.strip_prefix(&(k.to_owned() + " ")) {
            cmd = v.to_owned() + " " + c;
        }
    }
    cmd
}

pub async fn docker_export<W: Write>(tag: &String, out: &mut W) {
    docker()
        .create_container(
            Some(CreateContainerOptions {
                name: "ex",
                platform: None,
            }),
            Config {
                cmd: Some(vec!["sh"]),
                image: Some(tag),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let res = docker().export_container("ex");
    // Shouldn't load the whole file into memory, stream it to disk instead
    res.for_each(move |data| {
        out.write_all(&data.unwrap()).unwrap();
        ready(())
    })
    .await;
    docker().remove_container("ex", None).await.unwrap();
}

pub fn docker() -> Docker {
    Docker::connect_with_local_defaults().unwrap()
}

pub mod args;
pub mod assemble;
pub mod tar;