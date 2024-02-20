#![allow(clippy::iter_nth_zero)]
use bzip2::read::BzDecoder;
use maplit::btreemap;
use walkdir::WalkDir;
use async_recursion::async_recursion;
use bollard::{
    image::{BuildImageOptions, BuilderVersion},
    service::{BuildInfoAux, ImageId},
};
use bootstrapper_assemble::{
    alias,
    args::Args,
    docker, docker_export, download, emit_run, envify,
    tar::{ArchiveReader, TarArchiveReader, TarArchiveWriter, ZipArchiveReader},
};
use bootstrapper_common::recipe::{get_recipe_digest, NamedRecipeVersion, SourceContents, SOURCES};
use bytes::Bytes;
use clap::Parser;
use futures_util::StreamExt;
use indexmap::IndexMap;
use std::{
    collections::BTreeMap, io::{Cursor, Read, Write}, os::unix::fs::MetadataExt, path::PathBuf
};

const BUILD_CACHE_SOURCE_PATH: &str = "build-cache/source";
const BUILD_CACHE_OUT_PATH: &str = "build-cache/out";
const BUILD_CACHE_LINK_PATH: &str = "build-cache/link";

async fn build_source(name: &str) -> SourceContents {
    let source = SOURCES.get(name).unwrap();
    let source_path = PathBuf::from(BUILD_CACHE_SOURCE_PATH).join(&source.sha);

    if source_path.exists() {
        return source.clone();
    }

    println!("Downloading {}", source.url);
    let client = reqwest::Client::new();
    let srcdata = download(&client, &source.url).await.unwrap();
    assert_eq!(source.sha, sha256::digest(&srcdata));
    /*let src = std::io::Cursor::new(srcdata.clone());
    if source.extract.is_some() {
        if source.url.ends_with(".zip") {
            zip_extract::extract(src, &tempdir.join("ex"), true).unwrap();
        } else if source.url.ends_with(".tar.gz") {
            let tar = flate2::read::GzDecoder::new(src);
            let mut archive = tar::Archive::new(tar);
            archive.unpack(&tempdir.join("ex")).unwrap();
        } else {
            panic!("Unknown extension")
        }
    } else {
        std::fs::create_dir_all(tempdir.join("ex")).unwrap();
        std::fs::write(tempdir.join("ex").join("file"), srcdata).unwrap();
    }
    let mut builder = tar::Builder::new(std::fs::File::create(source_path).unwrap());
    builder.append_dir_all(".", tempdir.join("ex")).unwrap();
    builder.finish().unwrap();*/
    std::fs::File::create(source_path)
        .unwrap()
        .write_all(&srcdata)
        .unwrap();
    source.clone()
}

async fn build_image(context: Bytes) -> String {
    let d = docker();
    let mut bir = d.build_image(
        BuildImageOptions {
            t: "input",
            dockerfile: "Dockerfile",
            version: BuilderVersion::BuilderBuildKit,
            session: Some("a".to_owned()),
            ..Default::default()
        },
        None,
        Some(context.into()),
    );
    let mut iid = None;
    while let Some(v) = bir.next().await {
        let v = v.unwrap();
        if let Some(stream) = v.stream {
            print!("STM {}", stream);
        }
        if let Some(status) = v.status {
            print!("STS {}", status);
        }
        match v.aux {
            Some(BuildInfoAux::BuildKit(v)) => {
                for vx in v.vertexes {
                    println!("    {}", vx.name);
                }
            }
            Some(BuildInfoAux::Default(ImageId { id: Some(id) })) => iid = Some(id),
            v => todo!("{:?}", v),
        }
    }

    iid.unwrap()
}

async fn do_mkdirs(recipe: &NamedRecipeVersion, context_writer: &mut TarArchiveWriter<'_>) {
    if let Some(mkdirs) = &recipe.mkdirs {
        for mkdir in mkdirs {
            context_writer.create_empty_dir(mkdir.into()).unwrap();
        }
    }
}

async fn do_sources(recipe: &NamedRecipeVersion, context_writer: &mut TarArchiveWriter<'_>) {
    // Copy in our sources
    if let Some(source) = &recipe.source {
        for (name,source) in source {
            let sc = build_source(name).await;
            if let Some(extract) = &source.extract {
                assert!(source.chmod.is_none());
                let data = std::fs::read(format!("build-cache/source/{}", sc.sha)).unwrap();
                let mut reader = if sc.url.ends_with(".zip") {
                    let zar = ZipArchiveReader::from(data.as_slice());
                    ArchiveReader::ZIP(zar)
                } else if sc.url.ends_with(".tar.gz") {
                    todo!();
                    //let tar = flate2::read::GzDecoder::new(src);
                    //let mut archive = tar::Archive::new(tar);
                    //archive.unpack(&tempdir.join("ex")).unwrap();
                } else {
                    panic!("Unknown extension")
                };
                if let Some(copy) = &source.copy {
                    for i in copy {
                        let from = i.split(':').nth(0).unwrap();
                        let to = if let Some(to) = i.split(':').nth(1) {
                            to
                        } else {
                            from
                        };
                        context_writer.copy_from(
                            &mut reader,
                            from.into(),
                            PathBuf::from(extract).join(to),
                        );
                    }
                } else {
                    context_writer.copy_from(&mut reader, "".into(), PathBuf::from(extract));
                }
            } else if let Some(noextract) = &source.noextract {
                assert!(source.copy.is_none());
                context_writer
                    .create_file(
                        noextract.into(),
                        std::fs::read(format!("build-cache/source/{}", sc.sha))
                            .unwrap()
                            .as_ref(),
                        source.chmod
                            .as_ref()
                            .map(|x| u32::from_str_radix(&x, 8).unwrap()),
                    )
                    .unwrap();
            } else {
                todo!();
            }
        }
    }
}

async fn do_deps(recipe: &NamedRecipeVersion, context_writer: &mut TarArchiveWriter<'_>) {
    // Build and copy in our dependencies
    if let Some(deps) = &recipe.deps {
        for dep in deps {
            let dep_img = dep.split(':').nth(0).unwrap();
            let dep_ver = dep
                .split(':')
                .nth(1)
                .unwrap_or_else(|| panic!("Failed to find version in dep {}", dep));

            let dep_hash = get_recipe_digest(dep_img.to_owned(), dep_ver.to_owned());

            let content = std::fs::read(format!("build-cache/link/{}", dep_hash)).unwrap();

            //let tag = get_dep_tag(dep_img, dep_ver);
            if let Some(from_path) = dep.split(':').nth(2) {
                let to_path = dep.split(':').nth(3).unwrap();
                let mut tar = TarArchiveReader::from(content.as_ref());
                context_writer.copy_from_tar(&mut tar, from_path.into(), to_path.into());
            } else {
                let mut tar = TarArchiveReader::from(content.as_ref());
                context_writer.copy_from_tar(&mut tar, "".into(), "".into());
            }
        }
    }
}

async fn do_shell(
    recipe: &NamedRecipeVersion,
    context_writer: &mut TarArchiveWriter<'_>,
    dockerfile: &mut Cursor<Vec<u8>>,
) {
    // Load up a shell
    if let Some(shell) = &recipe.shell {
        let mut shell_it = shell.split(':');
        let shell_img = shell_it.next().unwrap();
        let shell_ver = shell_it.next().unwrap();
        /*let tag = get_dep_tag(shell_img, shell_ver);
        let shell = shell_it.next().unwrap();
        dockerfile
            .write_all(format!("COPY --from={} {} /bin/sh \n", tag, shell).as_bytes())
            .unwrap();*/
    }
}

async fn do_mods(
    recipe: &NamedRecipeVersion,
    context_writer: &mut TarArchiveWriter<'_>,
    dockerfile: &mut Cursor<Vec<u8>>,
) {
    let mods_path = PathBuf::from(format!("recipes/{}/{}", recipe.name, recipe.version));
    let mods_tar_path = PathBuf::from(format!("recipes/{}/{}.tar", recipe.name, recipe.version));

    // Copy in any mods
    if mods_tar_path.exists() {
        panic!();
        /*std::fs::copy(mods_tar_path, build_path.join("mod.tar")).unwrap();
        dockerfile.write_all(b"COPY ./mod.tar / \n").unwrap();
        dockerfile.write_all(b"RUN [\"tar\", \"xf\", \"/mod.tar\", \"--exclude=etc/resolv.conf\", \"--exclude=usr/bin/tar\", \"--exclude=bin\", \"--exclude=usr/sbin\"] \n").unwrap();
        */
    }
    if mods_path.exists() {
        for e in WalkDir::new(&mods_path) {
            let e = e.unwrap();
            let metadata = e.metadata().unwrap();
            if metadata.is_file() {
                println!("Applying mod {:?}",e.path().strip_prefix(&mods_path).unwrap().to_owned());
                context_writer.create_file(e.path().strip_prefix(&mods_path).unwrap().to_owned(), std::fs::read(e.path()).unwrap().as_ref(), Some(metadata.mode())).unwrap();
            }
        }
    }
}

async fn do_envs(
    dockerfile: &mut Cursor<Vec<u8>>,
    envs: Vec<PathBuf>,
    env: &mut IndexMap<String, String>,
) {
    // Load each envfile
    for envfile in envs {
        if envfile.exists() {
            for i in String::from_utf8(std::fs::read(envfile).unwrap())
                .unwrap()
                .split('\n')
            {
                if i.trim().is_empty() {
                    continue;
                }
                let k = i.split('=').nth(0).unwrap();
                let v = i.split('=').nth(1).unwrap().trim_matches('"');
                env.shift_remove(k);
                env.insert(k.to_owned(), v.to_owned());
            }
        }
    }

    // Write out our envs
    for (k, v) in env {
        dockerfile
            .write_all(format!("ENV {}=\"{}\"\r\n", k, v).as_bytes())
            .unwrap();
    }
}

async fn do_build(
    recipe: &NamedRecipeVersion,
    dockerfile: &mut Cursor<Vec<u8>>,
    env: &mut IndexMap<String, String>,
) {
    let mut stage = 0;
    let mut last_refresh = 0;
    let mut workdir = PathBuf::from("/");

    let mut aliases = IndexMap::new();
    // Run our build steps
    if let Some(compile) = &recipe.build.compile {
        for i in compile {
            let cmd =
                shlex::split(&alias(&envify(&i, &env), &aliases)).unwrap_or_else(|| panic!("Failed at line: {}", i));
            if cmd[0].contains('=') {
                let ev: Vec<_> = cmd[0].split('=').collect();
                assert_eq!(ev.len(), 2);
                env.insert(ev[0].to_owned(), ev[1].to_owned());
                dockerfile
                    .write_all(format!("ENV {}=\"{}\"\r\n", ev[0], ev[1]).as_bytes())
                    .unwrap();
            } else if cmd[0] == "cd" {
                assert_eq!(cmd.len(), 2);
                workdir = workdir.join(cmd[1].clone());
                dockerfile
                    .write_all(format!("WORKDIR {}\r\n", workdir.to_str().unwrap()).as_bytes())
                    .unwrap();
            } else if cmd[0] == "alias" {
                let (k,v) = cmd[1].split_once('=').unwrap();
                aliases.insert(k.to_owned(),v.to_owned());
            } else {
                emit_run(dockerfile, cmd, recipe.shell.is_some());
            }
            let newline_count = dockerfile.get_ref().iter().filter(|x| x==&&b'\n').count();
            if newline_count - last_refresh == 120 {
                dockerfile.write_all(format!("FROM scratch AS stage{}\r\n",stage+1).as_bytes()).unwrap();
                dockerfile.write_all(format!("COPY --from=stage{} / /\r\n",stage).as_bytes()).unwrap();
                for (k, v) in env.iter() {
                    dockerfile
                        .write_all(format!("ENV {}=\"{}\"\r\n", k, v).as_bytes())
                        .unwrap();
                }
                dockerfile
                    .write_all(format!("WORKDIR {}\r\n", workdir.to_str().unwrap()).as_bytes())
                    .unwrap();
                stage += 1;
                last_refresh = newline_count;
            }
        }
    }
}

async fn build_single(target: &str, version: &str, mut recipe: NamedRecipeVersion) -> String {
    println!(" Build requested of {} {}",target,version);
    let recipe_digest = get_recipe_digest(target.to_owned(), version.to_owned());

    let mods_path = PathBuf::from(format!("recipes/{}/{}", target, version));
    let envs = vec![mods_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("env")
        .to_owned()];

    let link_path = PathBuf::from(BUILD_CACHE_LINK_PATH).join(recipe_digest.clone());
    if link_path.exists() {
        println!("  Exists! {}",recipe_digest);
        return std::fs::read_link(link_path)
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
    }
    println!("Starting build for {}:{}", target, version);

    let mut env = IndexMap::new();

    if recipe.source.is_none() {
        recipe.source = Some(btreemap!{});
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

    let mut dockerfile = Cursor::new(Vec::new());
    dockerfile.write_all(b"FROM scratch AS stage0\n").unwrap();

    let mut context = Vec::new();
    let mut context_writer = TarArchiveWriter::from(context.as_mut());

    context_writer.create_file(PathBuf::from(".dockerignore"), b"Dockerfile", Some(0o644)).unwrap();

    do_mkdirs(&recipe, &mut context_writer).await;

    do_sources(&recipe, &mut context_writer).await;

    do_deps(&recipe, &mut context_writer).await;

    do_shell(&recipe, &mut context_writer, &mut dockerfile).await;

    do_mods(&recipe, &mut context_writer, &mut dockerfile).await;

    do_envs(&mut dockerfile, envs, &mut env).await;

    dockerfile.write_all(b"COPY . . \n").unwrap();

    do_build(&recipe, &mut dockerfile, &mut env).await;

    context_writer
        .create_file("./Dockerfile".into(), dockerfile.get_ref(), None)
        .unwrap();

    context_writer.finish().unwrap();

    std::mem::drop(context_writer);

    std::fs::write("t.tar", context.clone()).unwrap();

    let d = docker();
    let mut bir = d.build_image(
        BuildImageOptions {
            dockerfile: "Dockerfile",
            ..Default::default()
        },
        None,
        Some(context.into()),
    );
    let mut iid = None;
    while let Some(v) = bir.next().await {
        let v = v.unwrap();
        if let Some(stream) = v.stream {
            print!("{}", stream);
        }
        if let Some(BuildInfoAux::Default(ImageId { id: Some(id) })) = v.aux {
            iid = Some(id);
        }
    }
    let run_tag = iid.unwrap();

    let mut output_image = Vec::new();
    docker_export(&run_tag, &mut std::io::Cursor::new(&mut output_image)).await;

    std::fs::write("o.tar", output_image.clone()).unwrap();

    let mut tar = TarArchiveReader::from(output_image.as_slice());

    let mut output_clean = Vec::new();

    if recipe.artefacts.len() == 1 && recipe.artefacts[0].ends_with(".tar.bz2") {
        BzDecoder::new(Cursor::new(tar.file_contents(recipe.artefacts[0].clone().into()))).read_to_end(&mut output_clean).unwrap();
    } else {
        let mut taw = TarArchiveWriter::from(&mut output_clean);
        for art in &recipe.artefacts {
            taw.copy_from_tar(&mut tar, art.into(), art.into());
        }
        taw.finish().unwrap();
        std::mem::drop(taw);
    }

    let out_digest = sha256::digest(&output_clean);
    let out_path = PathBuf::from(BUILD_CACHE_OUT_PATH).join(&out_digest);

    std::fs::File::create(out_path)
        .unwrap()
        .write_all(&output_clean)
        .unwrap();

    std::os::unix::fs::symlink(format!("../out/{}", out_digest), link_path).unwrap();

    println!("  Finished build for {}:{}", target, version);
    println!("  {}->{}",recipe_digest,out_digest);

    out_digest
}

#[async_recursion]
async fn build_recipe(
    target: &str,
    version: &str,
    built: &mut BTreeMap<(String, String), String>,
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
            build_recipe(target_name, target_version, built).await;
        }
    }

    let hash_built = build_single(target, version, recipe).await;

    built.insert(
        (target.to_owned(), version.to_owned()),
        hash_built.to_owned(),
    );

    hash_built
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let target_name = args.target.split(':').nth(0).unwrap();
    let target_version = args.target.split(':').nth(1).unwrap();
    let mut built = BTreeMap::new();

    for cache in ["build-cache/source","build-cache/out","build-cache/link"] {
        let pb = PathBuf::from(cache);
        if !pb.exists() {
            std::fs::create_dir_all(pb).unwrap();
        }
    }

    build_recipe(target_name, target_version, &mut built).await;
}
