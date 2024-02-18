use clap::Parser;

/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Rebuild images for which we already have a container
    #[arg(short, long)]
    pub rebuild: bool,

    /// Compare before and after build steps
    #[arg(short, long)]
    pub diff: bool,

    /// Push built images to the registry
    #[arg(short, long)]
    pub push: bool,

    #[arg(last = true)]
    pub target: String,
}
