use clap::Parser;
use clio::Output;
use color_eyre::eyre::Context;
use rand::rngs::OsRng;
use russh::keys::ssh_key::{LineEnding, sec1::der::Writer};

/// Simple program to generate a new private key in the correct format
#[derive(Debug, Parser)]
#[clap(name = "generate_key")]
#[command(version, about, long_about = None)]
struct Args {
    #[clap(value_parser, default_value = "-")]
    output: Output,
}

fn main() -> color_eyre::Result<()> {
    let mut args = Args::parse();

    color_eyre::install()?;

    let key = russh::keys::PrivateKey::random(&mut OsRng, russh::keys::Algorithm::Ed25519)?;

    let key = key.to_openssh(LineEnding::LF)?;

    args.output
        .write(key.as_bytes())
        .wrap_err_with(|| format!("failed to write ssh key to output: {}", args.output.path()))?;

    Ok(())
}
