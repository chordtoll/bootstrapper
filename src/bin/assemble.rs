#![allow(clippy::iter_nth_zero)]

use async_recursion::async_recursion;
use bootstrapper::{Recipe, Source, helpers::{emit_run, download, envify, docker_export}};
use indexmap::IndexMap;
use std::{
    collections::{btree_map::Entry, BTreeMap},
    io::Write,
    path::PathBuf,
};


async fn build_source(source: &Source) {
    let o_push = std::env::args()
        .collect::<Vec<_>>()
        .contains(&"--push".to_string());
    
    let res = std::process::Command::new("docker")
        .args([
            "image",
            "inspect",
            &format!("bootstrapper/source:{}", source.sha),
        ])
        .output()
        .unwrap();
    if res.status.success() {
        return;
    }
    //println!("Trying to pull existing source image: {}",source.url);
    let res = std::process::Command::new("docker")
        .args(["pull", &format!("bootstrapper/source:{}", source.sha)])
        .status()
        .unwrap();
    if res.success() {
        return;
    }
    println!("Downloading {}", source.url);
    let tempdir = mktemp::Temp::new_dir().unwrap();
    let client = reqwest::Client::new();
    let srcdata = download(&client, &source.url).await.unwrap();
    assert_eq!(source.sha, sha256::digest(&srcdata));
    let src = std::io::Cursor::new(srcdata.clone());
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
    let mut f = std::fs::File::create(tempdir.join("Dockerfile")).unwrap();
    f.write_all(b"FROM scratch\r\nCOPY ./ex /\r\n").unwrap();
    let tag = &format!("bootstrapper/source:{}", source.sha);
    let path = tempdir.as_os_str().to_str().unwrap();
    assert!(std::process::Command::new("docker")
        .args(["buildx", "build", "--network", "none", "-t", tag, path])
        .status()
        .unwrap()
        .success());
    if o_push {
        assert!(std::process::Command::new("docker")
            .args(["push", &format!("bootstrapper/source:{}", source.sha),])
            .status()
            .unwrap()
            .success())
    }
}

#[async_recursion]
async fn build_recipe(
    target: &str,
    version: &str,
    built: &mut BTreeMap<(String, String), String>,
    toplevel: bool,
) -> String {
    let o_rebuild = std::env::args()
        .collect::<Vec<_>>()
        .contains(&"--rebuild".to_string());

    let o_diff = std::env::args()
        .collect::<Vec<_>>()
        .contains(&"--diff".to_string());

    let o_push = std::env::args()
        .collect::<Vec<_>>()
        .contains(&"--push".to_string());


    // Don't retry builds we've already done
    if let Some(tag) = built.get(&(target.to_owned(), version.to_owned())) {
        return tag.to_owned();
    }

    println!("Evaluating {}", target);

    // Set up our paths
    let recipe_path = PathBuf::from(format!("recipes/{}.yaml", target));
    let mods_path = PathBuf::from(format!("recipes/{}/{}", target, version));
    let build_path = PathBuf::from(format!("build/{}/{}", target, version));

    // Load recipe
    let mut recipe: Recipe =
        serde_yaml::from_reader(std::fs::File::open(recipe_path.clone()).unwrap()).unwrap();

    println!("Getting version {} from recipe {}", version, target);
    let mut recipe = recipe.remove(version).unwrap_or_else(|| {
        panic!(
            "No such version. try these: {:?}",
            recipe.keys().collect::<Vec<_>>()
        )
    });

    let u_str = Source {
        url: "https://github.com/chordtoll/static-utils/raw/develop/strace".to_owned(),
        sha: "a8d8c06d60f26e8ae75fac37415846811b3c4ebc261e246aeeffb5f7f8303e7b".to_owned(),
        extract: None,
        noextract: Some("/tmp/strace".to_owned()),
        copy: None,
        chmod: Some("555".to_owned()),
    };

    let bbox = Source {
        url: "https://busybox.net/downloads/binaries/1.35.0-x86_64-linux-musl/busybox".to_owned(),
        sha: "6e123e7f3202a8c1e9b1f94d8941580a25135382b99e8d3e34fb858bba311348".to_owned(),
        extract: None,
        noextract: Some("/tmp/busybox".to_owned()),
        copy: None,
        chmod: Some("555".to_owned()),
    };

    if let Some(v) = &mut recipe.source {
        v.insert(0, bbox);
        v.insert(0, u_str);
    } else {
        recipe.source = Some(vec![bbox, u_str]);
    }

    let tag_prep = &"bootstrapper/prep".to_string(); //let tag_prep = &format!("bootstrapper/build:{}-prep", target.replace('/', "."));
    let tag_build = &"bootstrapper/build".to_string(); //let tag_build = &format!("bootstrapper/build:{}-build", target.replace('/', "."));
    let tag_dist = &format!("bootstrapper/{}:{}", target.replace('/', "."), version);

    built.insert((target.to_owned(), version.to_owned()), tag_dist.to_owned());

    // Always rebuild the top level package, or every package if we've passed --rebuild
    if !toplevel && !o_rebuild {
        // Otherwise, don't try to rebuild dockerfiles that exist
        let res = std::process::Command::new("docker")
            .args(["image", "inspect", tag_dist])
            .output()
            .unwrap();
        if res.status.success() {
            return tag_dist.to_owned();
        }
    }

    // Make an empty folder for our mkdir directives
    std::fs::create_dir_all(build_path.join("empty")).unwrap();

    // Start our Dockerfile
    let mut f = std::fs::File::create(build_path.join(if o_diff {
        "Dockerfile.prep"
    } else {
        "Dockerfile.dist"
    }))
    .unwrap();
    f.write_all(b"FROM scratch as build\n").unwrap();

    // Figure out our env
    let mut env = IndexMap::new();

    // Load our two possible envfiles
    let envs = [recipe_path.parent().unwrap().join("env")];

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
    for (k, v) in &env {
        f.write_all(format!("ENV {}=\"{}\"\r\n", k, v).as_bytes())
            .unwrap();
    }

    // Create our mkdirs
    if let Some(mkdirs) = recipe.mkdirs {
        for mkdir in mkdirs {
            f.write_all(format!("COPY ./empty/ {} \n", mkdir).as_bytes())
                .unwrap();
        }
    }

    // Load up a shell
    if let Some(shell) = &recipe.shell {
        let mut shell_it = shell.split(':');
        let shell_img = shell_it.next().unwrap();
        let shell_ver = shell_it.next().unwrap();
        let tag = build_recipe(shell_img, shell_ver, built, false).await;
        let shell = shell_it.next().unwrap();
        f.write_all(format!("COPY --from={} {} /bin/sh \n", tag, shell).as_bytes())
            .unwrap();
    }

    // Build and copy in our dependencies
    if let Some(deps) = recipe.deps {
        for dep in deps {
            let dep_img = dep.split(':').nth(0).unwrap();
            let dep_ver = dep
                .split(':')
                .nth(1)
                .unwrap_or_else(|| panic!("Failed to find version in dep {}", dep));
            let tag = build_recipe(dep_img, dep_ver, built, false).await;
            if let Some(from_path) = dep.split(':').nth(2) {
                let to_path = dep.split(':').nth(3).unwrap();
                f.write_all(format!("COPY --from={} {} {} \n", tag, from_path, to_path).as_bytes())
                    .unwrap();
            } else {
                f.write_all(format!("COPY --from={} / / \n", tag,).as_bytes())
                    .unwrap();
            }
        }
    }

    // Copy in our sources
    if let Some(source) = recipe.source {
        for s in source {
            build_source(&s).await;
            if let Some(extract) = s.extract {
                assert!(s.chmod.is_none());
                if let Some(copy) = s.copy {
                    for i in copy {
                        let from = i.split(':').nth(0).unwrap();
                        let to = i.split(':').nth(1).unwrap();
                        f.write_all(
                            format!(
                                "COPY --from=bootstrapper/source:{} {} {} \n",
                                s.sha, from, to
                            )
                            .as_bytes(),
                        )
                        .unwrap();
                    }
                } else {
                    f.write_all(
                        format!("COPY --from=bootstrapper/source:{} / {} \n", s.sha, extract)
                            .as_bytes(),
                    )
                    .unwrap();
                }
            } else if let Some(noextract) = s.noextract {
                let chmod = if let Some(chmod) = s.chmod {
                    format!("--chmod={}", chmod)
                } else {
                    String::new()
                };
                f.write_all(
                    format!(
                        "COPY {} --from=bootstrapper/source:{} /file {} \n",
                        chmod, s.sha, noextract
                    )
                    .as_bytes(),
                )
                .unwrap();
            } else {
                todo!();
            }
        }
    }

    // Copy in any mods
    if mods_path.exists() {
        _ = std::fs::remove_dir_all(build_path.join("mod"));
        copy_dir::copy_dir(mods_path, build_path.join("mod")).unwrap();
        f.write_all(b"COPY ./mod/ / \n").unwrap();
    }

    let mut f = if o_diff {
        let mut f = std::fs::File::create(build_path.join("Dockerfile.build")).unwrap();
        f.write_all(format!("FROM {} AS build\n", tag_prep).as_bytes())
            .unwrap();
        f
    } else {
        f
    };

    // Run our build steps
    if let Some(compile) = recipe.build.compile {
        for i in compile {
            let cmd =
                shlex::split(&envify(&i, &env)).unwrap_or_else(|| panic!("Failed at line: {}", i));
            if cmd[0].contains('=') {
                let ev: Vec<_> = cmd[0].split('=').collect();
                assert_eq!(ev.len(), 2);
                env.insert(ev[0].to_owned(), ev[1].to_owned());
                f.write_all(format!("ENV {}=\"{}\"\r\n", ev[0], ev[1]).as_bytes())
                    .unwrap();
            } else if cmd[0] == "cd" {
                assert_eq!(cmd.len(), 2);
                f.write_all(format!("WORKDIR {}\r\n", cmd[1]).as_bytes())
                    .unwrap();
            } else {
                emit_run(&mut f, cmd, recipe.shell.is_some());
            }
        }
    }

    let mut f = if o_diff {
        // Create a new image for artefacts
        std::fs::File::create(build_path.join("Dockerfile.dist")).unwrap()
    } else {
        f
    };

    f.write_all(
        format!(
            "FROM {} as copy\n",
            if o_diff { tag_build } else { "build" }
        )
        .as_bytes(),
    )
    .unwrap();

    if recipe.artefacts.len() == 1 && recipe.artefacts[0].ends_with(".tar.bz2") {
        f.write_all(b"RUN [\"mkdir\", \"/out\"] \n").unwrap();
        f.write_all(
            format!(
                "RUN [\"bash\", \"-c\", \"bzip2 -dc {} | tar -xf - -C /out \"] \n",
                recipe.artefacts[0]
            )
            .as_bytes(),
        )
        .unwrap();
    } else {
        let mut ar_by_parent: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for i in recipe.artefacts {
            let p = PathBuf::from(i.clone())
                .parent()
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            match ar_by_parent.entry(p) {
                Entry::Occupied(mut o) => {
                    o.get_mut().push(i);
                }
                Entry::Vacant(v) => {
                    v.insert(vec![i]);
                }
            }
        }

        if !ar_by_parent.is_empty() {
            f.write_all(
                format!(
                    "RUN [\"/tmp/busybox\", \"mkdir\", \"-p\", {}] \n",
                    ar_by_parent
                        .keys()
                        .map(|x| format!("\"/out{}\"", x))
                        .collect::<Vec<String>>()
                        .join(",")
                )
                .as_bytes(),
            )
            .unwrap();
        }

        for (p, cs) in ar_by_parent.iter() {
            f.write_all(
                format!(
                    "RUN [\"/tmp/busybox\", \"cp\", \"-d\", \"-r\", {}\"/out{}\"] \n",
                    cs.iter()
                        .map(|x| format!("\"{}\",", x))
                        .collect::<Vec<String>>()
                        .join(" "),
                    p
                )
                .as_bytes(),
            )
            .unwrap();
        }
    }

    f.write_all(b"FROM scratch\n").unwrap();
    // Copy out our artefacts
    f.write_all(b"COPY --from=COPY /out /").unwrap();

    // We're done writing the Dockerfile, close it.
    std::mem::drop(f);

    // Actually do the build
    println!("Building {}:{}", target, version);

    if o_diff {
        std::fs::create_dir_all(PathBuf::from(format!("imex/{}", target)).parent().unwrap())
            .unwrap();

        let dp = build_path.join("Dockerfile.prep");
        let dps = dp.to_str().unwrap();
        let args = [
            "buildx",
            "build",
            "--network",
            "none",
            "-f",
            dps,
            "-t",
            tag_prep,
            build_path.as_os_str().to_str().unwrap(),
        ];
        //println!("{:?}",args.join(" "));
        assert!(std::process::Command::new("docker")
            .args(args)
            .status()
            .unwrap()
            .success());

        docker_export(tag_prep, format!("imex/{}:{}.pre.tar", target, version));

        let dp = build_path.join("Dockerfile.build");
        let dps = dp.to_str().unwrap();
        let args = [
            "buildx",
            "build",
            "--network",
            "none",
            "-f",
            dps,
            "-t",
            tag_build,
            build_path.as_os_str().to_str().unwrap(),
        ];
        //println!("{:?}",args.join(" "));
        assert!(std::process::Command::new("docker")
            .args(args)
            .status()
            .unwrap()
            .success());

        docker_export(tag_build, format!("imex/{}:{}.post.tar", target, version));
    }

    if o_diff {
        let args = [
            &format!("imex/{}:{}.pre.tar", target, version),
            &format!("imex/{}:{}.post.tar", target, version),
            "--html",
            &format!("imex/{}:{}.html", target, version),
            "--max-container-depth",
            "0",
            "--max-diff-block-lines",
            "10000",
            "--max-page-diff-block-lines",
            "10000",
        ];
        //println!("{:?}",args.join(" "));
        std::process::Command::new("diffoscope")
            .args(args)
            .status()
            .unwrap();
    }

    let dp = build_path.join("Dockerfile.dist");
    let dps = dp.to_str().unwrap();
    let args = [
        "buildx",
        "build",
        "--network",
        "none",
        "-f",
        dps,
        "-t",
        tag_dist,
        build_path.as_os_str().to_str().unwrap(),
    ];
    //println!("{:?}",args.join(" "));
    assert!(std::process::Command::new("docker")
        .args(args)
        .status()
        .unwrap()
        .success());

    if o_push {
        assert!(std::process::Command::new("docker")
        .args(["push", tag_dist])
        .status()
        .unwrap()
        .success());
    }

    tag_dist.to_owned()
}

#[tokio::main]
async fn main() {
    let target = std::env::args().nth(1).unwrap();
    let target_name = target.split(':').nth(0).unwrap();
    let target_version = target.split(':').nth(1).unwrap();
    let mut built = BTreeMap::new();
    build_recipe(target_name, target_version, &mut built, true).await;
}
