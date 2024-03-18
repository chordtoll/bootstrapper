use std::{
    collections::BTreeMap,
    fs::File,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
};

use lazy_static::lazy_static;

use serde::Deserialize;

use memoize::memoize;

lazy_static! {
    static ref RECIPE_CACHE: lockfree::map::Map<PathBuf, Recipe> = lockfree::map::Map::new();
    pub static ref SOURCES: BTreeMap<String, SourceContents> = load_sources();
}

pub fn load_sources() -> BTreeMap<String, SourceContents> {
    serde_yaml::from_reader::<File, BTreeMap<String, SourceContents>>(
        File::open("sources.yaml").unwrap(),
    )
    .unwrap()
}

#[memoize]
pub fn get_recipe_digest(target: String, version: String, additional_salt: &'static str) -> String {
    let recipe = NamedRecipeVersion::load_by_target_version(&target, &version);

    let mods_path = PathBuf::from(format!("recipes/{}/{}", target, version));
    let envs = vec![mods_path
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("env")
        .to_owned()];

    let mut envs_summary = String::new();
    for i in &envs {
        let env = std::fs::read_to_string(i).unwrap_or(String::new());
        envs_summary.push_str(&format!("{} {}", env.len(), env));
    }

    let summary = recipe.summary(additional_salt);

    if additional_salt.is_empty() {
        sha256::digest(format!(
            "{} {}/{} {}",
            summary.len(),
            summary,
            envs_summary.len(),
            envs_summary
        ))
    } else {
        sha256::digest(format!(
            "{} {}/{} {}/{} {}",
            summary.len(),
            summary,
            envs_summary.len(),
            envs_summary,
            additional_salt.len(),
            additional_salt,
        ))
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct Source {
    pub extract: Option<String>,
    pub noextract: Option<String>,
    pub copy: Option<Vec<String>>,
    pub chmod: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SourceContents {
    pub url: String,
    pub sha: String,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum RecipeBuildSteps {
    Single {
        single: Vec<String>,
    },
    Piecewise {
        unpack: Option<Vec<String>>,
        unpack_dirname: String,
        patch_dir: String,
        package_dir: Option<String>,
        prepare: Option<Vec<String>>,
        configure: Option<Vec<String>>,
        compile: Option<Vec<String>>,
        install: Option<Vec<String>>,
        postprocess: Option<Vec<String>>,
    },
}

#[derive(Debug, Deserialize, Clone)]
pub struct RecipeVersion {
    pub source: Option<BTreeMap<String, Source>>,
    pub shell: Option<String>,
    pub deps: Option<Vec<String>>,
    pub mkdirs: Option<Vec<String>>,
    pub build: RecipeBuildSteps,
    pub artefacts: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct NamedRecipeVersion {
    pub name: String,
    pub version: String,
    pub source: Option<BTreeMap<String, Source>>,
    pub shell: Option<String>,
    pub deps: Option<Vec<String>>,
    pub mkdirs: Option<Vec<String>>,
    pub build: RecipeBuildSteps,
    pub artefacts: Vec<String>,
}

#[derive(Clone)]
pub struct Recipe(BTreeMap<String, RecipeVersion>);

impl Recipe {
    pub fn load_by_name(name: &str) -> Self {
        Self::load(&PathBuf::from(format!("recipes/{}.yaml", name)))
    }
    pub fn load(path: &Path) -> Self {
        if let Some(recipe) = RECIPE_CACHE.get(path) {
            return recipe.1.clone();
        }
        let res = match serde_yaml::from_reader::<File, Self>(File::open(path).unwrap()) {
            Ok(v) => v,
            Err(e) => panic!(
                "Parse error in YAML at {}:{}:{} ({:?})",
                path.to_str().unwrap(),
                e.location().unwrap().line(),
                e.location().unwrap().column(),
                e
            ),
        };
        RECIPE_CACHE.insert(path.to_owned(), res.clone());
        res
    }
}

impl Deref for Recipe {
    type Target = BTreeMap<String, RecipeVersion>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for Recipe {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<'de> Deserialize<'de> for Recipe {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Self(<BTreeMap<String, RecipeVersion>>::deserialize(
            deserializer,
        )?))
    }
}

impl NamedRecipeVersion {
    pub fn load_by_name(name: &str) -> Self {
        let (target, version) = name.split_once(':').unwrap();
        Self::load_by_target_version(target, version)
    }
    pub fn load_by_target_version(target: &str, version: &str) -> Self {
        let mut recipe = Recipe::load_by_name(target);

        let rv = recipe.remove(version).unwrap_or_else(|| {
            panic!(
                "No such version {}:{}. try these: {:?}",
                target,
                version,
                recipe.keys().collect::<Vec<_>>()
            )
        });
        NamedRecipeVersion {
            name: target.to_owned(),
            version: version.to_owned(),
            source: rv.source,
            shell: rv.shell,
            deps: rv.deps,
            mkdirs: rv.mkdirs,
            build: rv.build,
            artefacts: rv.artefacts,
        }
    }

    pub fn summary(&self, additional_salt: &'static str) -> String {
        let mut inputs = String::new();
        if let Some(sources) = &self.source {
            for (name, source) in sources {
                let summary = source.summary(name);
                inputs.push_str(&format!("SOURCE:{}\n{}", summary.len(), summary));
            }
        }
        if let Some(shell) = &self.shell {
            inputs.push_str(&format!("SHELL:{}\n {}\n", shell.len(), shell));
        }
        if let Some(deps) = &self.deps {
            for i in deps {
                let target = i.split(':').nth(0).unwrap();
                let version = i.split(':').nth(1).unwrap();
                let from = i.split(':').nth(2);
                let to = i.split(':').nth(3);
                inputs.push_str(&format!(
                    "DEP:{} {:?} {:?}\n",
                    get_recipe_digest(target.to_owned(), version.to_owned(), additional_salt),
                    from,
                    to
                ));
            }
        }
        if let Some(mkdirs) = &self.mkdirs {
            for i in mkdirs {
                inputs.push_str(&format!("MKDIR:{}\n {}\n", i.len(), i));
            }
        }
        let build_summary = self.build.summary();
        inputs.push_str(&format!("BUILD:{}\n{}", build_summary.len(), build_summary));
        for i in &self.artefacts {
            inputs.push_str(&format!(" ARTEFACT:{}\n  {}\n", i.len(), i));
        }
        inputs
    }
}

impl RecipeBuildSteps {
    pub fn summary(&self) -> String {
        let mut inputs = String::new();
        match self {
            Self::Single { single } => {
                for i in single {
                    inputs.push_str(&format!(" SINGLE:{}\n  {}\n", i.len(), i));
                }
            }
            Self::Piecewise {
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
                if let Some(unpack) = unpack {
                    for i in unpack {
                        inputs.push_str(&format!(" UNPACK:{}\n  {}\n", i.len(), i));
                    }
                }
                inputs.push_str(&format!(
                    " DIRNAME={}\n  {}\n",
                    unpack_dirname.len(),
                    unpack_dirname
                ));
                inputs.push_str(&format!(
                    " PATCH_DIR={}\n  {}\n",
                    patch_dir.len(),
                    patch_dir
                ));
                if let Some(package_dir) = package_dir {
                    inputs.push_str(&format!(
                        " PACKAGE_DIR={}\n  {}\n",
                        package_dir.len(),
                        package_dir
                    ));
                }
                if let Some(prepare) = prepare {
                    for i in prepare {
                        inputs.push_str(&format!(" PREPARE:{}\n  {}\n", i.len(), i));
                    }
                }
                if let Some(configure) = configure {
                    for i in configure {
                        inputs.push_str(&format!(" CONFIGURE:{}\n  {}\n", i.len(), i));
                    }
                }
                if let Some(compile) = compile {
                    for i in compile {
                        inputs.push_str(&format!(" COMPILE:{}\n  {}\n", i.len(), i));
                    }
                }
                if let Some(install) = install {
                    for i in install {
                        inputs.push_str(&format!(" INSTALL:{}\n  {}\n", i.len(), i));
                    }
                }
                if let Some(postprocess) = postprocess {
                    for i in postprocess {
                        inputs.push_str(&format!(" POSTPROCESS:{}\n  {}\n", i.len(), i));
                    }
                }
            }
        }
        inputs
    }
}

impl Source {
    pub fn summary(&self, name: &str) -> String {
        let mut inputs = String::new();
        let sha = &SOURCES
            .get(name)
            .unwrap_or_else(|| panic!("Unknown source {}", name))
            .sha;
        inputs.push_str(&format!(" SHA:{}\n  {}\n", sha.len(), sha.to_lowercase()));
        if let Some(extract) = &self.extract {
            inputs.push_str(&format!(" EXTRACT:{}\n  {}\n", extract.len(), extract));
        }
        if let Some(noextract) = &self.noextract {
            inputs.push_str(&format!(
                " NOEXTRACT:{}\n  {}\n",
                noextract.len(),
                noextract
            ));
        }
        if let Some(copy) = &self.copy {
            let mut copy_str = String::new();
            for c in copy {
                copy_str.push_str(&format!("  {} {}", c.len(), c));
            }
            inputs.push_str(&format!(" COPY:{}\n{}\n", copy_str.len(), copy_str));
        }
        if let Some(chmod) = &self.chmod {
            inputs.push_str(&format!(" CHMOD:{}\n  {}\n", chmod.len(), chmod));
        }
        inputs
    }
}
