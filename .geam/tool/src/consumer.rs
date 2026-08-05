use std::{
    fs::{self, File},
    path::Path,
    process::Command,
};

use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;
use serde_json::Value as JsonValue;
use tar::Archive;
use tempfile::TempDir;
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

use crate::{
    config::ReleaseConfig,
    package::{PackageArtifacts, write_verification},
};

pub fn verify(
    mirror_root: &Path,
    geam_root: &Path,
    target_dir: Option<&Path>,
    config: &ReleaseConfig,
    artifacts: &PackageArtifacts,
) -> Result<()> {
    ensure!(
        geam_root.join("Cargo.toml").is_file(),
        "Geam Cargo.toml is missing"
    );
    let temp = TempDir::new().context("could not create consumer verification directory")?;
    let package_root = temp.path().join("packages");
    fs::create_dir(&package_root).context("could not create extracted package directory")?;
    extract_packages(mirror_root, &package_root, artifacts)?;

    let geam_archive = temp.path().join("geam.tar");
    let status = Command::new("git")
        .args(["archive", "--format=tar", "--output"])
        .arg(&geam_archive)
        .arg("HEAD")
        .current_dir(geam_root)
        .status()
        .context("could not archive Geam")?;
    ensure!(status.success(), "git archive failed for Geam");
    let consumer_root = temp.path().join("geam");
    fs::create_dir(&consumer_root).context("could not create Geam consumer directory")?;
    Archive::new(File::open(&geam_archive)?).unpack(&consumer_root)?;

    rewrite_consumer_manifest(&consumer_root, &package_root, config)?;
    let lock = consumer_root.join("Cargo.lock");
    if lock.exists() {
        fs::remove_file(&lock).context("could not remove Geam git dependency lock")?;
    }

    run_cargo(
        &consumer_root,
        target_dir,
        &["generate-lockfile"],
        "generate Geam consumer lockfile",
    )?;
    verify_consumer_metadata(&consumer_root, target_dir, config)?;
    run_cargo(
        &consumer_root,
        target_dir,
        &["test", "--locked", "--quiet"],
        "test packaged Geam consumer",
    )?;
    run_cargo(
        &consumer_root,
        target_dir,
        &[
            "clippy",
            "--locked",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        "clippy packaged Geam consumer",
    )?;

    let mut verified = artifacts.clone();
    verified.geam_commit = Some(git_head(geam_root)?);
    write_verification(mirror_root, &verified)
}

fn extract_packages(
    mirror_root: &Path,
    package_root: &Path,
    artifacts: &PackageArtifacts,
) -> Result<()> {
    for artifact in &artifacts.packages {
        let path = mirror_root.join(&artifact.file);
        let decoder = GzDecoder::new(
            File::open(&path).with_context(|| format!("could not open {}", path.display()))?,
        );
        Archive::new(decoder)
            .unpack(package_root)
            .with_context(|| format!("could not extract {}", path.display()))?;
    }
    Ok(())
}

fn rewrite_consumer_manifest(
    consumer_root: &Path,
    package_root: &Path,
    config: &ReleaseConfig,
) -> Result<()> {
    let manifest_path = consumer_root.join("Cargo.toml");
    let source = fs::read_to_string(&manifest_path).context("could not read Geam Cargo.toml")?;
    let mut document = source
        .parse::<DocumentMut>()
        .context("could not parse Geam Cargo.toml")?;
    let core = config
        .package_by_published_name("geam-gleam-core")
        .context("core package is missing from release config")?;
    let mut dependency = InlineTable::new();
    dependency.insert("package", Value::from(&core.published_name));
    dependency.insert(
        "version",
        Value::from(format!("={}", config.package_version()?)),
    );
    document["dependencies"]["gleam-core"] = Item::Value(Value::InlineTable(dependency));

    if document.get("patch").is_none() {
        document["patch"] = Item::Table(Table::new());
    }
    if document["patch"].get("crates-io").is_none() {
        document["patch"]["crates-io"] = Item::Table(Table::new());
    }
    for package in &config.packages {
        let extracted = package_root.join(format!(
            "{}-{}",
            package.published_name,
            config.package_version()?
        ));
        ensure!(
            extracted.join("Cargo.toml").is_file(),
            "extracted package is missing: {}",
            extracted.display()
        );
        let mut patch = InlineTable::new();
        patch.insert("path", Value::from(extracted.to_string_lossy().as_ref()));
        document["patch"]["crates-io"][&package.published_name] =
            Item::Value(Value::InlineTable(patch));
    }
    fs::write(&manifest_path, document.to_string()).context("could not write Geam Cargo.toml")
}

fn verify_consumer_metadata(
    consumer_root: &Path,
    target_dir: Option<&Path>,
    config: &ReleaseConfig,
) -> Result<()> {
    let mut command = Command::new("cargo");
    command
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(consumer_root);
    if let Some(target_dir) = target_dir {
        command.env("CARGO_TARGET_DIR", target_dir);
    }
    let output = command
        .output()
        .context("could not read Geam consumer metadata")?;
    ensure!(
        output.status.success(),
        "Geam consumer metadata failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let metadata: JsonValue = serde_json::from_slice(&output.stdout)?;
    let packages = metadata["packages"]
        .as_array()
        .context("Geam consumer metadata packages are missing")?;
    verify_resolved_packages(packages, config)
}

fn verify_resolved_packages(packages: &[JsonValue], config: &ReleaseConfig) -> Result<()> {
    let expected_version = config.package_version()?.to_string();
    for expected in &config.packages {
        ensure!(
            packages.iter().any(|package| {
                package["name"] == expected.published_name && package["version"] == expected_version
            }),
            "Geam consumer did not resolve {} at {}",
            expected.published_name,
            expected_version
        );
        ensure!(
            !packages
                .iter()
                .any(|package| package["name"] == expected.source_name),
            "Geam consumer still resolved upstream package {}",
            expected.source_name
        );
    }
    Ok(())
}

fn run_cargo(
    root: &Path,
    target_dir: Option<&Path>,
    arguments: &[&str],
    operation: &str,
) -> Result<()> {
    let mut command = Command::new("cargo");
    command.args(arguments).current_dir(root);
    if let Some(target_dir) = target_dir {
        command.env("CARGO_TARGET_DIR", target_dir);
    }
    let status = command
        .status()
        .with_context(|| format!("could not {operation}"))?;
    ensure!(status.success(), "failed to {operation}");
    Ok(())
}

fn git_head(root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .context("could not read Geam HEAD")?;
    ensure!(
        output.status.success(),
        "git rev-parse HEAD failed for Geam"
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PackageConfig;

    fn config() -> ReleaseConfig {
        ReleaseConfig {
            upstream_repository: "gleam-lang/gleam".into(),
            upstream_tag: "v1.18.1".into(),
            upstream_commit: "a".repeat(40),
            revision: 1,
            repository: "https://github.com/panarch/gleam".into(),
            dependency_pins: vec![],
            packages: vec![PackageConfig {
                path: "compiler-core".into(),
                source_name: "gleam-core".into(),
                published_name: "geam-gleam-core".into(),
                crate_name: "gleam_core".into(),
                description: "Compiler core".into(),
            }],
        }
    }

    #[test]
    fn rewrites_only_the_compiler_dependency_and_registry_patches() {
        let temp = TempDir::new().unwrap();
        fs::write(
            temp.path().join("Cargo.toml"),
            r#"[package]
name = "geam"
version = "0.1.0"

[dependencies]
gleam-core = { git = "https://example.com", rev = "abc" }
"#,
        )
        .unwrap();
        let package_root = temp.path().join("packages");
        let package_path = package_root.join("geam-gleam-core-1.18.1-geam.1");
        fs::create_dir_all(&package_path).unwrap();
        fs::write(package_path.join("Cargo.toml"), "[package]\nname='core'\n").unwrap();
        let config = config();

        rewrite_consumer_manifest(temp.path(), &package_root, &config).unwrap();

        let source = fs::read_to_string(temp.path().join("Cargo.toml")).unwrap();
        let document = source.parse::<DocumentMut>().unwrap();
        assert_eq!(
            document["dependencies"]["gleam-core"]["package"].as_str(),
            Some("geam-gleam-core")
        );
        assert_eq!(
            document["dependencies"]["gleam-core"]["version"].as_str(),
            Some("=1.18.1-geam.1")
        );
        assert!(
            document["patch"]["crates-io"]["geam-gleam-core"]["path"]
                .as_str()
                .unwrap()
                .ends_with("geam-gleam-core-1.18.1-geam.1")
        );
    }

    #[test]
    fn requires_the_exact_mirrored_package_and_rejects_the_upstream_identity() {
        let mirrored = vec![serde_json::json!({
            "name": "geam-gleam-core",
            "version": "1.18.1-geam.1"
        })];
        verify_resolved_packages(&mirrored, &config()).unwrap();

        let upstream = vec![serde_json::json!({
            "name": "gleam-core",
            "version": "1.18.1"
        })];
        assert_eq!(
            verify_resolved_packages(&upstream, &config())
                .unwrap_err()
                .to_string(),
            "Geam consumer did not resolve geam-gleam-core at 1.18.1-geam.1"
        );

        let mixed = vec![
            serde_json::json!({
                "name": "geam-gleam-core",
                "version": "1.18.1-geam.1"
            }),
            serde_json::json!({
                "name": "gleam-core",
                "version": "1.18.1"
            }),
        ];
        assert_eq!(
            verify_resolved_packages(&mixed, &config())
                .unwrap_err()
                .to_string(),
            "Geam consumer still resolved upstream package gleam-core"
        );
    }
}
