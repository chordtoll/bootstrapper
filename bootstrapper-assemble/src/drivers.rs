use bootstrapper_common::recipe::NamedRecipeVersion;

use crate::args::Args;

pub mod buildkit;
pub mod docker;

pub trait BuildDriver {
    async fn run(
        &mut self,
        recipe: &NamedRecipeVersion,
        additional_salt: &'static str,
        args: &Args,
    ) -> Vec<u8>;
}
