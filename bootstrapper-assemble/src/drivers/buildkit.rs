use std::{
    collections::HashMap,
    io::{Cursor, Write},
    os::unix::fs::MetadataExt,
    path::PathBuf,
    process::Stdio,
};

use bollard::{
    image::BuildImageOptions,
    secret::{BuildInfoAux, ImageId},
};
use bootstrapper_common::recipe::{get_recipe_digest, NamedRecipeVersion, RecipeBuildSteps};
use buildkit_llb::ops::{OperationBuilder, SingleBorrowedOutput};
use buildkit_proto::pb::{Definition, OpMetadata};
use futures_util::StreamExt;
use indexmap::IndexMap;
use maplit::hashmap;
use prost::Message;
use regex::Regex;
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, TempDir};
use walkdir::WalkDir;

use crate::{
    alias,
    args::Args,
    assemble::build_source,
    docker, docker_export, envify,
    tar::{flatten_tar, ArchiveReader, TarArchiveReader, TarArchiveWriter, ZipArchiveReader},
};

use super::BuildDriver;

#[derive(Debug)]
struct Command {
    args: Vec<String>,
    cwd: String,
    envs: HashMap<String, String>,
}

pub struct BuildkitDriver {
    commands: Vec<Command>,
    context_writer: Option<TarArchiveWriter>,
    env: IndexMap<String, String>,
    workdir: PathBuf,
}

fn op_encode(op: buildkit_proto::pb::Op) -> (Vec<u8>, String) {
    let mut oe = Vec::new();
    op.encode(&mut oe).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(&oe);
    let hash = format!("sha256:{:x}", hasher.finalize());
    (oe, hash)
}

impl BuildkitDriver {
    pub fn new() -> Self {
        let context_writer = TarArchiveWriter::from(Vec::new());
        Self {
            commands: Vec::new(),
            context_writer: Some(context_writer),
            env: IndexMap::new(),
            workdir: PathBuf::from("/"),
        }
    }

    pub fn emit_run(&mut self, cmd: Vec<String>, shell: bool) {
        /*let cmd = if shell {
            format!("RUN {}", cmd.join(" "))
        } else {
            format!("RUN [\"{}\"]", cmd.join("\",\""))
        };*/
        self.commands.push(Command {
            args: cmd,
            envs: self
                .env
                .iter()
                .map(|(a, b)| (a.clone(), b.clone()))
                .collect(),
            cwd: self.workdir.to_str().unwrap().to_owned(),
        });
        //self.dockerfile.write_all(cmd.as_bytes()).unwrap();
        //self.dockerfile.write_all(b" \n").unwrap();
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

    pub fn do_envs(&mut self, envs: Vec<PathBuf>) {
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
                    self.env.shift_remove(k);
                    self.env.insert(k.to_owned(), v.to_owned());
                }
            }
        }
    }

    pub fn emit_steps(&mut self, recipe: &NamedRecipeVersion, steps: &Vec<String>) {
        let mut aliases = IndexMap::new();

        for i in steps {
            let cmd = shlex::split(&alias(&envify(i, &self.env), &aliases))
                .unwrap_or_else(|| panic!("Failed at line: {}", i));
            if cmd[0].contains('=') {
                let ev: Vec<_> = cmd[0].split('=').collect();
                assert_eq!(ev.len(), 2);
                self.env.insert(ev[0].to_owned(), ev[1].to_owned());
            } else if cmd[0] == "cd" {
                assert_eq!(cmd.len(), 2);
                self.workdir = path_clean::clean(self.workdir.join(cmd[1].clone()));
            } else if cmd[0] == "alias" {
                let (k, v) = cmd[1].split_once('=').unwrap();
                aliases.insert(k.to_owned(), v.to_owned());
            } else {
                self.emit_run(cmd, recipe.shell.is_some());
            }
            /*let newline_count = self
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
            }*/
        }
    }

    pub fn do_build(&mut self, args: &Args, recipe: &NamedRecipeVersion) {
        // Run our build steps
        match &recipe.build {
            RecipeBuildSteps::Single { single } => {
                self.emit_steps(recipe, single);
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
                self.emit_steps(recipe, &steps);
            }
        }
    }
    fn graph_encode(&self) -> Vec<u8> {
        let mut def = Vec::new();
        let mut metadata = HashMap::new();

        let initial_op = buildkit_proto::pb::Op {
            constraints: None,
            inputs: Vec::new(),
            op: Some(buildkit_proto::pb::op::Op::Source(
                buildkit_proto::pb::SourceOp {
                    identifier: "local://context".to_string(),
                    attrs: HashMap::new(),
                },
            )),
            platform: None,
        };

        let (oe, hash) = op_encode(initial_op);
        let mut prev_op_hash = hash.clone();

        def.push(oe);
        metadata.insert(
        hash,
        OpMetadata {
            caps: HashMap::new(),
            ignore_cache: false,
            description: hashmap!{"llb.customname".to_string()=> "Starting with generated build context".to_string()},
            export_cache: None,
        });

        for cmd in &self.commands {
            let op = buildkit_proto::pb::Op {
                constraints: None,
                inputs: vec![buildkit_proto::pb::Input {
                    digest: prev_op_hash.clone(),
                    index: 0,
                }],
                op: Some(buildkit_proto::pb::op::Op::Exec(
                    buildkit_proto::pb::ExecOp {
                        meta: Some(buildkit_proto::pb::Meta {
                            args: cmd.args.clone(),
                            env: cmd
                                .envs
                                .iter()
                                .map(|(k, v)| {
                                    format!(
                                        "{}={}",
                                        k,
                                        envify(
                                            v,
                                            &cmd.envs
                                                .iter()
                                                .map(|(a, b)| (a.clone(), b.clone()))
                                                .collect()
                                        )
                                    )
                                })
                                .collect(),
                            cwd: cmd.cwd.clone(),
                            user: String::new(),
                            proxy_env: None,
                            extra_hosts: Vec::new(),
                        }),
                        mounts: vec![buildkit_proto::pb::Mount {
                            input: 0,
                            selector: String::new(),
                            dest: "/".to_string(),
                            output: 0,
                            readonly: false,
                            mount_type: 0,
                            cache_opt: None,
                            secret_opt: None,
                            ssh_opt: None,
                        }],
                        network: 2,
                        security: 0,
                    },
                )),
                platform: None,
            };

            let (oe, hash) = op_encode(op.clone());
            prev_op_hash = hash.clone();

            def.push(oe);

            let summary = if let Some(buildkit_proto::pb::op::Op::Exec(v)) = op.op {
                format!(
                    "\tCMD: {}\n\tCWD: {}\n\tENV:{:?}",
                    v.meta.as_ref().unwrap().args.join(" "),
                    v.meta.as_ref().unwrap().cwd,
                    v.meta.as_ref().unwrap().env
                )
            } else {
                todo!();
            };

            metadata.insert(
                hash,
                OpMetadata {
                    caps: HashMap::new(),
                    ignore_cache: false,
                    description: hashmap! {"llb.customname".to_string()=> summary},
                    export_cache: None,
                },
            );
        }

        let op = buildkit_proto::pb::Op {
            constraints: None,
            inputs: vec![buildkit_proto::pb::Input {
                digest: prev_op_hash.clone(),
                index: 0,
            }],
            op: None,
            platform: None,
        };

        let (oe, hash) = op_encode(op);
        prev_op_hash = hash.clone();

        def.push(oe);
        metadata.insert(
            hash,
            OpMetadata {
                caps: HashMap::new(),
                ignore_cache: false,
                description: hashmap! {"llb.customname".to_string()=> "Final step".to_string()},
                export_cache: None,
            },
        );

        let mut b = Vec::new();
        let defin = Definition { def, metadata };
        defin.encode(&mut b).unwrap();
        b
    }
}

impl BuildDriver for BuildkitDriver {
    async fn run(
        &mut self,
        recipe: &NamedRecipeVersion,
        additional_salt: &'static str,
        args: &Args,
    ) -> Vec<u8> {
        let builder_image = buildkit_llb::prelude::Source::local("context")
            .custom_name("Starting with generated build context");

        let mods_path = PathBuf::from(format!("recipes/{}/{}", recipe.name, recipe.version));
        let envs = vec![mods_path
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("env")
            .to_owned()];

        self.do_mkdirs(&recipe);

        self.do_sources(&recipe).await;

        self.do_deps(&recipe, additional_salt);

        self.do_shell(&recipe);

        self.do_mods(&recipe);

        self.do_envs(envs);

        self.do_build(args, &recipe);

        self.context_writer.as_mut().unwrap().finish().unwrap();

        let cw = std::mem::take(&mut self.context_writer);

        let context = flatten_tar(&cw.unwrap().take());

        let in_dir = TempDir::new().unwrap();

        std::fs::write("i.tar", context.clone()).unwrap();

        tar::Archive::new(Cursor::new(context))
            .unpack(in_dir.path())
            .unwrap();

        let out_tar = NamedTempFile::new().unwrap();

        let mut process = std::process::Command::new("buildctl")
            .arg("build")
            .arg("--local")
            .arg(format!("context={}", in_dir.path().to_str().unwrap()))
            .arg("--output")
            .arg(format!(
                "type=tar,dest={}",
                out_tar.path().to_str().unwrap()
            ))
            .arg("--progress=plain")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();

        let llb = self.graph_encode();

        std::fs::write("llb", llb.clone()).unwrap();

        process
            .stdin
            .as_mut()
            .unwrap()
            .write_all(llb.as_slice())
            .unwrap();

        let status = process.wait().unwrap();

        assert!(status.success());

        let output = std::fs::read(out_tar.path()).unwrap();

        std::fs::write("o.tar", output.clone()).unwrap();

        output
    }
}
