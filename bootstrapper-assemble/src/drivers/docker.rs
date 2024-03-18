use std::{
    io::{Cursor, Write},
    os::unix::fs::MetadataExt,
    path::PathBuf,
};

use bollard::{
    image::{BuildImageOptions, BuilderVersion},
    secret::{BuildInfoAux, ImageId},
};
use bootstrapper_common::recipe::{self, get_recipe_digest, NamedRecipeVersion, RecipeBuildSteps};
use futures_util::StreamExt;
use indexmap::IndexMap;
use regex::Regex;
use walkdir::WalkDir;

use crate::{
    alias,
    args::Args,
    assemble::build_source,
    docker, docker_export, envify,
    tar::{flatten_tar, ArchiveReader, TarArchiveReader, TarArchiveWriter, ZipArchiveReader},
};

use super::BuildDriver;

pub struct DockerDriver {
    context_writer: Option<TarArchiveWriter>,
    dockerfile: Cursor<Vec<u8>>,
}

impl DockerDriver {
    pub fn new() -> Self {
        let mut dockerfile_buf = Vec::new();
        let mut dockerfile = Cursor::new(dockerfile_buf);
        dockerfile.write_all(b"FROM scratch AS stage0\n").unwrap();

        let mut context_writer = TarArchiveWriter::from(Vec::new());

        context_writer
            .create_file(PathBuf::from(".dockerignore"), b"Dockerfile", Some(0o644))
            .unwrap();
        Self {
            dockerfile,
            context_writer: Some(context_writer),
        }
    }

    pub fn emit_run(&mut self, cmd: Vec<String>, shell: bool) {
        let cmd = if shell {
            format!("RUN {}", cmd.join(" "))
        } else {
            format!("RUN [\"{}\"]", cmd.join("\",\""))
        };
        self.dockerfile.write_all(cmd.as_bytes()).unwrap();
        self.dockerfile.write_all(b" \n").unwrap();
    }

    pub fn do_mkdirs(&mut self, recipe: &NamedRecipeVersion) {
        if let Some(mkdirs) = &recipe.mkdirs {
            for mkdir in mkdirs {
                self.context_writer
                    .as_mut()
                    .unwrap()
                    .create_empty_dir(mkdir.into())
                    .unwrap();
            }
        }
    }

    pub async fn do_sources(&mut self, recipe: &NamedRecipeVersion) {
        // Copy in our sources
        if let Some(source) = &recipe.source {
            for (name, source) in source {
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
                            self.context_writer.as_mut().unwrap().copy_from(
                                &mut reader,
                                from.into(),
                                PathBuf::from(extract).join(to),
                            );
                        }
                    } else {
                        self.context_writer.as_mut().unwrap().copy_from(
                            &mut reader,
                            "".into(),
                            PathBuf::from(extract),
                        );
                    }
                } else if let Some(noextract) = &source.noextract {
                    assert!(source.copy.is_none());
                    self.context_writer
                        .as_mut()
                        .unwrap()
                        .create_file(
                            noextract.into(),
                            std::fs::read(format!("build-cache/source/{}", sc.sha))
                                .unwrap()
                                .as_ref(),
                            source
                                .chmod
                                .as_ref()
                                .map(|x| u32::from_str_radix(x, 8).unwrap()),
                        )
                        .unwrap();
                } else {
                    todo!();
                }
            }
        }
    }

    pub fn do_deps(&mut self, recipe: &NamedRecipeVersion, additional_salt: &'static str) {
        // Build and copy in our dependencies
        if let Some(deps) = &recipe.deps {
            for dep in deps {
                let dep_img = dep.split(':').nth(0).unwrap();
                let dep_ver = dep
                    .split(':')
                    .nth(1)
                    .unwrap_or_else(|| panic!("Failed to find version in dep {}", dep));

                let dep_hash =
                    get_recipe_digest(dep_img.to_owned(), dep_ver.to_owned(), additional_salt);

                let content = std::fs::read(format!("build-cache/link/{}", dep_hash)).unwrap();

                //let tag = get_dep_tag(dep_img, dep_ver);
                if let Some(from_path) = dep.split(':').nth(2) {
                    let to_path = dep.split(':').nth(3).unwrap();
                    let mut tar = TarArchiveReader::from(content.as_ref());
                    self.context_writer.as_mut().unwrap().copy_from_tar(
                        &mut tar,
                        from_path.into(),
                        to_path.into(),
                    );
                } else {
                    let mut tar = TarArchiveReader::from(content.as_ref());
                    self.context_writer.as_mut().unwrap().copy_from_tar(
                        &mut tar,
                        "".into(),
                        "".into(),
                    );
                }
            }
        }
    }

    pub fn do_shell(&mut self, recipe: &NamedRecipeVersion) {
        // Load up a shell
        if let Some(shell) = &recipe.shell {
            let mut shell_it = shell.split(':');
            let _shell_img = shell_it.next().unwrap();
            let _shell_ver = shell_it.next().unwrap();
            todo!();
            /*let tag = get_dep_tag(shell_img, shell_ver);
            let shell = shell_it.next().unwrap();
            dockerfile
                .write_all(format!("COPY --from={} {} /bin/sh \n", tag, shell).as_bytes())
                .unwrap();*/
        }
    }

    pub fn do_mods(&mut self, recipe: &NamedRecipeVersion) {
        let mods_path = PathBuf::from(format!("recipes/{}/{}", recipe.name, recipe.version));
        let mods_tar_path =
            PathBuf::from(format!("recipes/{}/{}.tar", recipe.name, recipe.version));

        // Copy in any mods
        if mods_tar_path.exists() {
            todo!();
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
                    println!(
                        "Applying mod {:?}",
                        e.path().strip_prefix(&mods_path).unwrap().to_owned()
                    );
                    self.context_writer
                        .as_mut()
                        .unwrap()
                        .create_file(
                            e.path().strip_prefix(&mods_path).unwrap().to_owned(),
                            std::fs::read(e.path()).unwrap().as_ref(),
                            Some(metadata.mode()),
                        )
                        .unwrap();
                }
            }
        }
    }

    pub fn do_envs(&mut self, envs: Vec<PathBuf>, env: &mut IndexMap<String, String>) {
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
            self.dockerfile
                .write_all(format!("ENV {}=\"{}\"\r\n", k, v).as_bytes())
                .unwrap();
        }
    }

    pub fn emit_steps(
        &mut self,
        recipe: &NamedRecipeVersion,
        steps: &Vec<String>,
        env: &mut IndexMap<String, String>,
    ) {
        let mut stage = 0;
        let mut last_refresh = 0;
        let mut workdir = PathBuf::from("/");

        let mut aliases = IndexMap::new();

        for i in steps {
            let cmd = shlex::split(&alias(&envify(i, env), &aliases))
                .unwrap_or_else(|| panic!("Failed at line: {}", i));
            if cmd[0].contains('=') {
                let ev: Vec<_> = cmd[0].split('=').collect();
                assert_eq!(ev.len(), 2);
                env.insert(ev[0].to_owned(), ev[1].to_owned());
                self.dockerfile
                    .write_all(format!("ENV {}=\"{}\"\r\n", ev[0], ev[1]).as_bytes())
                    .unwrap();
            } else if cmd[0] == "cd" {
                assert_eq!(cmd.len(), 2);
                workdir = path_clean::clean(workdir.join(cmd[1].clone()));
                self.dockerfile
                    .write_all(format!("WORKDIR {}\r\n", workdir.to_str().unwrap()).as_bytes())
                    .unwrap();
            } else if cmd[0] == "alias" {
                let (k, v) = cmd[1].split_once('=').unwrap();
                aliases.insert(k.to_owned(), v.to_owned());
            } else {
                self.emit_run(cmd, recipe.shell.is_some());
            }
            let newline_count = self
                .dockerfile
                .get_ref()
                .iter()
                .filter(|x| x == &&b'\n')
                .count();
            if newline_count - last_refresh == 120 {
                self.dockerfile
                    .write_all(format!("FROM scratch AS stage{}\r\n", stage + 1).as_bytes())
                    .unwrap();
                self.dockerfile
                    .write_all(format!("COPY --from=stage{} / /\r\n", stage).as_bytes())
                    .unwrap();
                for (k, v) in env.iter() {
                    self.dockerfile
                        .write_all(format!("ENV {}=\"{}\"\r\n", k, v).as_bytes())
                        .unwrap();
                }
                self.dockerfile
                    .write_all(format!("WORKDIR {}\r\n", workdir.to_str().unwrap()).as_bytes())
                    .unwrap();
                stage += 1;
                last_refresh = newline_count;
            }
        }
    }

    pub fn do_build(
        &mut self,
        args: &Args,
        recipe: &NamedRecipeVersion,
        env: &mut IndexMap<String, String>,
    ) {
        self.dockerfile.write_all(b"COPY . . \n").unwrap();

        // Run our build steps
        match &recipe.build {
            RecipeBuildSteps::Single { single } => {
                self.emit_steps(recipe, single, env);
            }
            RecipeBuildSteps::Piecewise {
                unpack,
                unpack_dirname,
                patch_dir,
                package_dir,
                prepare,
                configure,
                compile,
                install,
                postprocess,
            } => {
                let mut steps = Vec::new();
                let (pkg, pass) = if let Some(v) = Regex::new(r"^(.*)-pass([0-9]+)")
                    .unwrap()
                    .captures(&recipe.version)
                {
                    let pass: u32 = v.get(2).unwrap().as_str().parse().unwrap();
                    (
                        format!(
                            "{}-{}",
                            recipe.name.split('/').last().unwrap(),
                            v.get(1).unwrap().as_str()
                        ),
                        pass - 1,
                    )
                } else {
                    (
                        format!(
                            "{}-{}",
                            recipe.name.split('/').last().unwrap(),
                            recipe.version
                        ),
                        0,
                    )
                };
                let pkg = if let Some(package_dir) = package_dir {
                    package_dir.to_owned()
                } else {
                    pkg
                };
                steps.push(format!("pkg={}", pkg));
                steps.push(format!("cd /steps/{}", pkg));
                steps.push(format!("base_dir=/steps/{}", pkg));
                steps.push(format!("patch_dir=/steps/{}/{}", pkg, patch_dir));
                steps.push(format!("mk_dir=/steps/{}/mk", pkg));
                steps.push(format!("files_dir=/steps/{}/files", pkg));
                steps.push(format!("revision={}", pass));
                steps.push("mkdir build".to_owned());
                steps.push("cd build".to_owned());
                if let Some(_unpack) = unpack {
                    todo!();
                } else {
                    steps.push("bash -exc '. /steps/helpers.sh; default_src_unpack'".to_owned());
                    steps.push(format!("dirname={}", unpack_dirname));
                    steps.push(format!("cd {}", unpack_dirname));
                }
                if let Some(prepare) = prepare {
                    for i in prepare {
                        if i == "default" {
                            steps.push(
                                "bash -exc '. /steps/helpers.sh; default_src_prepare'".to_owned(),
                            );
                        } else {
                            steps.push(i.to_owned());
                        }
                    }
                } else {
                    steps.push("bash -exc '. /steps/helpers.sh; default_src_prepare'".to_owned());
                }
                if let Some(configure) = configure {
                    for i in configure {
                        if i == "default" {
                            steps.push(
                                "bash -exc '. /steps/helpers.sh; default_src_configure'".to_owned(),
                            );
                        } else {
                            steps.push(i.to_owned());
                        }
                    }
                } else {
                    steps.push("bash -exc '. /steps/helpers.sh; default_src_configure'".to_owned());
                }
                if let Some(compile) = compile {
                    for i in compile {
                        if i == "default" {
                            steps.push(
                                "bash -exc '. /steps/helpers.sh; default_src_compile'".to_owned(),
                            );
                        } else {
                            steps.push(i.to_owned());
                        }
                    }
                } else {
                    steps.push("bash -exc '. /steps/helpers.sh; default_src_compile'".to_owned());
                }
                if let Some(install) = install {
                    for i in install {
                        if i == "default" {
                            steps.push(
                                "bash -exc '. /steps/helpers.sh; default_src_install'".to_owned(),
                            );
                        } else {
                            steps.push(i.to_owned());
                        }
                    }
                } else {
                    steps.push("bash -exc '. /steps/helpers.sh; default_src_install'".to_owned());
                }
                if let Some(postprocess) = postprocess {
                    for i in postprocess {
                        if i == "default" {
                            steps.push(
                                "bash -exc '. /steps/helpers.sh; default_src_postprocess'"
                                    .to_owned(),
                            );
                        } else {
                            steps.push(i.to_owned());
                        }
                    }
                } else {
                    steps.push(
                        "bash -exc '. /steps/helpers.sh; default_src_postprocess'".to_owned(),
                    );
                }
                steps.push("cd ${DESTDIR}".to_owned());
                steps.push("bash -exc '. /steps/helpers.sh; src_pkg'".to_owned());
                steps.push("cd /external/repo".to_owned());
                if !args.no_checksum {
                    steps.push(
                        "bash -exc '. /steps/helpers.sh; src_checksum ${pkg} ${revision}'"
                            .to_owned(),
                    );
                }
                self.emit_steps(recipe, &steps, env);
            }
        }
    }
}

impl BuildDriver for DockerDriver {
    async fn run(
        &mut self,
        recipe: &NamedRecipeVersion,
        additional_salt: &'static str,
        args: &Args,
    ) -> Vec<u8> {
        let mods_path = PathBuf::from(format!("recipes/{}/{}", recipe.name, recipe.version));
        let envs = vec![mods_path
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("env")
            .to_owned()];

        let mut env = IndexMap::new();

        self.do_mkdirs(&recipe);

        self.do_sources(&recipe).await;

        self.do_deps(&recipe, additional_salt);

        self.do_shell(&recipe);

        self.do_mods(&recipe);

        self.do_envs(envs, &mut env);

        self.do_build(args, &recipe, &mut env);

        self.context_writer
            .as_mut()
            .unwrap()
            .create_file("./Dockerfile".into(), self.dockerfile.get_ref(), None)
            .unwrap();

        self.context_writer.as_mut().unwrap().finish().unwrap();

        let cw = std::mem::take(&mut self.context_writer);

        let context = flatten_tar(&cw.unwrap().take());

        std::fs::write("t.tar", context.clone()).unwrap();

        let d = docker();
        let mut bir = d.build_image(
            BuildImageOptions {
                dockerfile: "Dockerfile",
                networkmode: "none",
                //version: BuilderVersion::BuilderBuildKit,
                //session: Some(String::from("sess")),
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
        output_image
    }
}
