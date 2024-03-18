#![allow(clippy::iter_nth_zero)]
#![warn(clippy::unused_async)]
use crate::drivers::BuildDriver;
use crate::{
    args::Args,
    download,
    drivers::docker::DockerDriver,
    tar::{TarArchiveReader, TarArchiveWriter},
};
use async_recursion::async_recursion;
use bootstrapper_common::recipe::{get_recipe_digest, NamedRecipeVersion, SourceContents, SOURCES};
use bzip2::read::BzDecoder;
use indexmap::IndexMap;
use maplit::btreemap;
use std::{
    collections::BTreeMap,
    io::{Cursor, ErrorKind, Read, Write},
    path::PathBuf,
};

const BUILD_CACHE_SOURCE_PATH: &str = "build-cache/source";
const BUILD_CACHE_OUT_PATH: &str = "build-cache/out";
const BUILD_CACHE_LINK_PATH: &str = "build-cache/link";

pub async fn build_source(name: &str) -> SourceContents {
    let source = SOURCES.get(name).unwrap();
    let source_path = PathBuf::from(BUILD_CACHE_SOURCE_PATH).join(&source.sha);

    if source_path.exists() {
        return source.clone();
    }

    println!("Downloading {}", source.url);
    let client = reqwest::Client::new();
    let srcdata = download(&client, &source.url).await.unwrap();
    assert_eq!(source.sha, sha256::digest(&srcdata));
    std::fs::File::create(source_path)
        .unwrap()
        .write_all(&srcdata)
        .unwrap();
    source.clone()
}

async fn build_single(
    args: &Args,
    target: &str,
    version: &str,
    mut recipe: NamedRecipeVersion,
    force: bool,
    additional_salt: &'static str,
) -> String {
    println!(" Build requested of {} {}", target, version);
    let recipe_digest = get_recipe_digest(target.to_owned(), version.to_owned(), additional_salt);

    let link_path = PathBuf::from(BUILD_CACHE_LINK_PATH).join(recipe_digest.clone());
    if link_path.exists() && !force {
        println!("  Exists! {}", recipe_digest);
        return std::fs::read_link(link_path)
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
    }
    println!("Starting build for {}:{}", target, version);

    if recipe.source.is_none() {
        recipe.source = Some(btreemap! {});
    }

    /*let u_str = Source {
        url: "https://github.com/chordtoll/static-utils/raw/develop/strace".to_owned(),
        sha: "a8d8c06d60f26e8ae75fac37415846811b3c4ebc261e246aeeffb5f7f8303e7b".to_owned(),
        extract: None,
        noextract: Some("/tmp/strace".to_owned()),
        copy: None,
        chmod: Some("555".to_owned()),
    };
    recipe.source.as_mut().unwrap().push(u_str);*/

    /*let bbox = Source {
        url: "https://busybox.net/downloads/binaries/1.35.0-x86_64-linux-musl/busybox".to_owned(),
        sha: "6e123e7f3202a8c1e9b1f94d8941580a25135382b99e8d3e34fb858bba311348".to_owned(),
        extract: None,
        noextract: Some("/tmp/busybox".to_owned()),
        copy: None,
        chmod: Some("555".to_owned()),
    };

    recipe.source.as_mut().unwrap().push(bbox);*/

    let mut driver = DockerDriver::new();

    let output_image = driver.run(&recipe, additional_salt, args).await;

    std::fs::write("o.tar", output_image.clone()).unwrap();

    let mut tar = TarArchiveReader::from(output_image.as_slice());

    let mut taw = TarArchiveWriter::from(Vec::new());

    let artefacts = recipe.artefacts.clone();

    if artefacts.len() > 0 && recipe.artefacts[0].ends_with(".tar.bz2") {
        println!("Extracting archive");
        let mut buf = Vec::new();
        BzDecoder::new(Cursor::new(
            tar.file_contents(recipe.artefacts[0].clone().into()),
        ))
        .read_to_end(&mut buf)
        .unwrap();
        let mut tar = TarArchiveReader::from(buf.as_slice());
        taw.copy_from_tar(&mut tar, PathBuf::new(), PathBuf::new());
        //artefacts.remove(0);
    } else {
        println!("Not extracting archive");
    }
    tar.reset();
    for art in &artefacts {
        taw.copy_from_tar(&mut tar, art.into(), art.into());
    }
    taw.finish().unwrap();

    let output_clean = taw.take();

    let out_digest = sha256::digest(&output_clean);
    let out_path = PathBuf::from(BUILD_CACHE_OUT_PATH).join(&out_digest);

    std::fs::File::create(out_path)
        .unwrap()
        .write_all(&output_clean)
        .unwrap();

    match std::os::unix::fs::symlink(format!("../out/{}", out_digest), link_path.clone()) {
        Ok(_) => {}
        Err(v) => {
            if v.kind() == ErrorKind::AlreadyExists {
                std::fs::remove_file(link_path.clone()).unwrap();
                std::os::unix::fs::symlink(format!("../out/{}", out_digest), link_path).unwrap();
            }
        }
    }

    println!("  Finished build for {}:{}", target, version);
    println!("  {}->{}", recipe_digest, out_digest);

    out_digest
}

#[async_recursion]
pub async fn build_recipe(
    args: &Args,
    target: &str,
    version: &str,
    built: &mut BTreeMap<(String, String), String>,
    force: bool,
    additional_salt: &'static str,
) -> String {
    // Don't retry builds we've already done
    if let Some(hash_built) = built.get(&(target.to_owned(), version.to_owned())) {
        return hash_built.to_owned();
    }

    println!("Evaluating {}", target);

    let recipe = NamedRecipeVersion::load_by_target_version(target, version);

    if let Some(deps) = &recipe.deps {
        for i in deps {
            let target_name = i.split(':').nth(0).unwrap();
            let target_version = i.split(':').nth(1).unwrap();
            build_recipe(
                args,
                target_name,
                target_version,
                built,
                false,
                additional_salt,
            )
            .await;
        }
    }

    let hash_built = build_single(args, target, version, recipe, force, additional_salt).await;

    built.insert(
        (target.to_owned(), version.to_owned()),
        hash_built.to_owned(),
    );

    hash_built
}
