mod config;
mod lockfile;
mod manifest;
mod package;
mod repository;
mod source;

use std::{env, path::PathBuf};

use anyhow::{Context, Result, bail};
use config::ReleaseConfig;

fn main() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .context("packaging tool is not under .geam/tool")?
        .to_path_buf();
    let config_path = root.join(".geam/release.toml");
    let mut arguments = env::args().skip(1);
    let Some(command) = arguments.next() else {
        bail!("expected apply, verify, set-release, metadata, or package");
    };

    match command.as_str() {
        "apply" => {
            expect_end(arguments)?;
            let config = ReleaseConfig::load(&config_path)?;
            repository::apply(&root)?;
            manifest::apply(&root, &config)?;
            source::apply(&root, &config)?;
        }
        "verify" => {
            expect_end(arguments)?;
            let config = ReleaseConfig::load(&config_path)?;
            repository::verify(&root, &config)?;
            manifest::verify(&root, &config)?;
            source::verify(&root, &config)?;
            lockfile::verify(&root, &config)?;
            package::verify_metadata(&root, &config)?;
        }
        "set-release" => {
            let tag = next_option(&mut arguments, "--tag")?;
            let commit = next_option(&mut arguments, "--commit")?;
            let revision = next_option(&mut arguments, "--revision")?
                .parse()
                .context("revision is not an integer")?;
            expect_end(arguments)?;

            let mut config = ReleaseConfig::load(&config_path)?;
            config.set_release(tag, commit, revision)?;
            config.save(&config_path)?;
            repository::apply(&root)?;
            manifest::apply(&root, &config)?;
            source::apply(&root, &config)?;
        }
        "metadata" => {
            expect_end(arguments)?;
            let config = ReleaseConfig::load(&config_path)?;
            println!("upstream_repository={}", config.upstream_repository);
            println!("upstream_tag={}", config.upstream_tag);
            println!("upstream_commit={}", config.upstream_commit);
            println!("revision={}", config.revision);
            println!("package_version={}", config.package_version()?);
            println!("release_tag={}", config.release_tag()?);
        }
        "package" => {
            expect_end(arguments)?;
            let config = ReleaseConfig::load(&config_path)?;
            repository::verify(&root, &config)?;
            manifest::verify(&root, &config)?;
            source::verify(&root, &config)?;
            lockfile::verify(&root, &config)?;
            package::build(&root, &config)?;
        }
        _ => bail!("unknown command: {command}"),
    }

    Ok(())
}

fn next_option(arguments: &mut impl Iterator<Item = String>, expected: &str) -> Result<String> {
    let option = arguments
        .next()
        .with_context(|| format!("missing {expected}"))?;
    if option != expected {
        bail!("expected {expected}, found {option}");
    }
    arguments
        .next()
        .with_context(|| format!("missing value for {expected}"))
}

fn expect_end(mut arguments: impl Iterator<Item = String>) -> Result<()> {
    if let Some(argument) = arguments.next() {
        bail!("unexpected argument: {argument}");
    }
    Ok(())
}
