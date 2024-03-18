use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    #[arg(short, long)]
    pub rebuild: bool,

    #[arg(short, long)]
    pub diff: bool,

    #[arg(short, long)]
    pub push: bool,

    #[arg(short, long)]
    pub no_checksum: bool,

    #[arg(last = true)]
    pub target: Option<String>,
}
