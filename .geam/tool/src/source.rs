use std::{fs, path::Path};

use anyhow::{Context, Result, ensure};

use crate::{config::ReleaseConfig, repository};

const COMPILER_VERSION_SOURCE: &str = "compiler-core/src/version.rs";
const UPSTREAM_VERSION_DECLARATION: &str =
    "pub const COMPILER_VERSION: &str = env!(\"CARGO_PKG_VERSION\");";
const HEXPM_SOURCE: &str = "hexpm/src/lib.rs";
const UPSTREAM_USER_AGENT_DECLARATION: &str =
    "static USER_AGENT: &str = concat!(\"Gleam v\", env!(\"CARGO_PKG_VERSION\"));";

pub fn apply(root: &Path, config: &ReleaseConfig) -> Result<()> {
    write_expected(
        root,
        COMPILER_VERSION_SOURCE,
        expected_compiler_version_source(root, config)?,
    )?;
    write_expected(root, HEXPM_SOURCE, expected_hexpm_source(root, config)?)
}

pub fn verify(root: &Path, config: &ReleaseConfig) -> Result<()> {
    verify_expected(
        root,
        COMPILER_VERSION_SOURCE,
        expected_compiler_version_source(root, config)?,
    )?;
    verify_expected(root, HEXPM_SOURCE, expected_hexpm_source(root, config)?)?;
    Ok(())
}

fn write_expected(root: &Path, relative_path: &str, expected: String) -> Result<()> {
    let path = root.join(relative_path);
    fs::write(&path, expected).with_context(|| format!("could not write {}", path.display()))
}

fn verify_expected(root: &Path, relative_path: &str, expected: String) -> Result<()> {
    let path = root.join(relative_path);
    let actual =
        fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?;
    ensure!(
        actual == expected,
        "{relative_path} does not preserve the upstream release version"
    );
    Ok(())
}

fn expected_compiler_version_source(root: &Path, config: &ReleaseConfig) -> Result<String> {
    let upstream =
        repository::read_tagged_file(root, &config.upstream_tag, COMPILER_VERSION_SOURCE)?;
    transform_compiler_version(&upstream, &config.upstream_version()?.to_string())
}

fn expected_hexpm_source(root: &Path, config: &ReleaseConfig) -> Result<String> {
    let upstream = repository::read_tagged_file(root, &config.upstream_tag, HEXPM_SOURCE)?;
    transform_hexpm_user_agent(&upstream, &config.upstream_version()?.to_string())
}

fn transform_compiler_version(source: &str, version: &str) -> Result<String> {
    ensure!(
        source.matches(UPSTREAM_VERSION_DECLARATION).count() == 1,
        "upstream compiler version declaration changed"
    );
    Ok(source.replacen(
        UPSTREAM_VERSION_DECLARATION,
        &format!("pub const COMPILER_VERSION: &str = \"{version}\";"),
        1,
    ))
}

fn transform_hexpm_user_agent(source: &str, version: &str) -> Result<String> {
    ensure!(
        source.matches(UPSTREAM_USER_AGENT_DECLARATION).count() == 1,
        "upstream Hex user-agent declaration changed"
    );
    Ok(source.replacen(
        UPSTREAM_USER_AGENT_DECLARATION,
        &format!("static USER_AGENT: &str = \"Gleam v{version}\";"),
        1,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separates_the_upstream_compiler_version_from_the_package_revision() {
        let source = format!("// version owner\n{UPSTREAM_VERSION_DECLARATION}\n");

        assert_eq!(
            transform_compiler_version(&source, "1.18.1").unwrap(),
            "// version owner\npub const COMPILER_VERSION: &str = \"1.18.1\";\n"
        );
    }

    #[test]
    fn rejects_an_unrecognized_upstream_declaration() {
        assert_eq!(
            transform_compiler_version("pub const VERSION: &str = \"1\";", "1.18.1")
                .unwrap_err()
                .to_string(),
            "upstream compiler version declaration changed"
        );
    }

    #[test]
    fn separates_the_upstream_hex_user_agent_from_the_package_revision() {
        let source = format!("// HTTP owner\n{UPSTREAM_USER_AGENT_DECLARATION}\n");

        assert_eq!(
            transform_hexpm_user_agent(&source, "1.18.1").unwrap(),
            "// HTTP owner\nstatic USER_AGENT: &str = \"Gleam v1.18.1\";\n"
        );
    }
}
