use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use tar::Archive;
use toml::Value as TomlValue;
use toml_edit::{DocumentMut, InlineTable, Item, Table, Value};

use crate::config::{PackageConfig, ReleaseConfig};

const CRATES_IO_MAX_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PackageArtifacts {
    pub upstream_tag: String,
    pub upstream_commit: String,
    pub mirror_commit: String,
    pub package_version: String,
    pub geam_commit: Option<String>,
    pub packages: Vec<PackageArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PackageArtifact {
    pub name: String,
    pub file: PathBuf,
    pub sha256: String,
    pub size: u64,
}

pub fn verify_metadata(root: &Path, config: &ReleaseConfig) -> Result<()> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--locked"])
        .current_dir(root)
        .output()
        .context("could not run cargo metadata")?;
    ensure!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let metadata: JsonValue =
        serde_json::from_slice(&output.stdout).context("cargo metadata returned invalid JSON")?;
    let packages = metadata["packages"]
        .as_array()
        .context("cargo metadata packages are missing")?;
    let version = config.package_version()?.to_string();

    for package in &config.packages {
        ensure!(
            !packages
                .iter()
                .any(|metadata| metadata["name"] == package.source_name),
            "source package name remains in cargo metadata: {}",
            package.source_name
        );
        let metadata = packages
            .iter()
            .find(|metadata| metadata["name"] == package.published_name)
            .with_context(|| format!("missing package metadata for {}", package.published_name))?;
        ensure!(
            metadata["version"] == version,
            "wrong package version for {}",
            package.published_name
        );
        let targets = metadata["targets"]
            .as_array()
            .context("package targets are missing")?;
        ensure!(
            targets.iter().any(|target| {
                target["name"] == package.crate_name
                    && target["kind"]
                        .as_array()
                        .is_some_and(|kinds| kinds.iter().any(|kind| kind == "lib"))
            }),
            "Rust library name was not preserved for {}",
            package.published_name
        );
    }
    Ok(())
}

pub fn build(root: &Path, config: &ReleaseConfig) -> Result<PackageArtifacts> {
    let target = root.join(".geam/target");
    let package_dir = target.join("package");
    fs::create_dir_all(&package_dir).context("could not create package output directory")?;
    let version = config.package_version()?.to_string();
    let mut packages = Vec::new();

    for package in &config.packages {
        let artifact = package_dir.join(format!("{}-{version}.crate", package.published_name));
        if artifact.exists() {
            fs::remove_file(&artifact)
                .with_context(|| format!("could not remove stale {}", artifact.display()))?;
        }
        let patch_config = write_local_patches(root, &target, config, package)?;
        let mut command = Command::new("cargo");
        command
            .args([
                "package",
                "--package",
                &package.published_name,
                "--allow-dirty",
                "--no-verify",
                "--locked",
                "--target-dir",
            ])
            .arg(&target);
        if let Some(patch_config) = patch_config {
            command.args(["--config"]).arg(patch_config);
        }
        let status = command
            .current_dir(root)
            .status()
            .with_context(|| format!("could not package {}", package.published_name))?;
        ensure!(
            status.success(),
            "cargo package failed for {}",
            package.published_name
        );
        ensure!(
            artifact.is_file(),
            "cargo did not create {}",
            artifact.display()
        );

        let size = fs::metadata(&artifact)?.len();
        ensure!(
            size <= CRATES_IO_MAX_BYTES,
            "{} exceeds the crates.io package limit",
            artifact.display()
        );
        inspect_archive(&artifact, package, config)?;
        packages.push(PackageArtifact {
            name: package.published_name.clone(),
            file: artifact
                .strip_prefix(root)
                .expect("artifact is under repository root")
                .to_path_buf(),
            sha256: sha256(&artifact)?,
            size,
        });
    }

    let artifacts = PackageArtifacts {
        upstream_tag: config.upstream_tag.clone(),
        upstream_commit: config.upstream_commit.clone(),
        mirror_commit: git_head(root)?,
        package_version: version,
        geam_commit: None,
        packages,
    };
    let verification_path = package_dir.join("geam-verification.json");
    fs::write(
        &verification_path,
        serde_json::to_vec_pretty(&artifacts).context("could not encode artifact metadata")?,
    )
    .with_context(|| format!("could not write {}", verification_path.display()))?;
    Ok(artifacts)
}

fn write_local_patches(
    root: &Path,
    target: &Path,
    config: &ReleaseConfig,
    owner: &PackageConfig,
) -> Result<Option<PathBuf>> {
    let dependencies = local_mirror_dependencies(root, config, owner)?;
    if dependencies.is_empty() {
        return Ok(None);
    }

    let mut document = DocumentMut::new();
    document["patch"] = Item::Table(Table::new());
    document["patch"]["crates-io"] = Item::Table(Table::new());
    for package in dependencies {
        let mut patch = InlineTable::new();
        patch.insert(
            "path",
            Value::from(root.join(&package.path).to_string_lossy().as_ref()),
        );
        document["patch"]["crates-io"][&package.published_name] =
            Item::Value(Value::InlineTable(patch));
    }

    let path = target.join(format!("local-patches-{}.toml", owner.published_name));
    fs::write(&path, document.to_string())
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok(Some(path))
}

fn local_mirror_dependencies<'a>(
    root: &Path,
    config: &'a ReleaseConfig,
    owner: &PackageConfig,
) -> Result<Vec<&'a PackageConfig>> {
    let path = root.join(&owner.path).join("Cargo.toml");
    let manifest: TomlValue = toml::from_str(
        &fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?,
    )
    .with_context(|| format!("could not parse {}", path.display()))?;
    let mut names = BTreeSet::new();
    collect_local_dependency_names(&manifest, config, &mut names);
    Ok(config
        .packages
        .iter()
        .filter(|package| names.contains(&package.published_name))
        .collect())
}

fn collect_local_dependency_names(
    manifest: &TomlValue,
    config: &ReleaseConfig,
    names: &mut BTreeSet<String>,
) {
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(dependencies) = manifest.get(section).and_then(TomlValue::as_table) {
            collect_local_dependency_table(dependencies, config, names);
        }
    }
    if let Some(targets) = manifest.get("target").and_then(TomlValue::as_table) {
        for target in targets.values().filter_map(TomlValue::as_table) {
            for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(dependencies) = target.get(section).and_then(TomlValue::as_table) {
                    collect_local_dependency_table(dependencies, config, names);
                }
            }
        }
    }
}

fn collect_local_dependency_table(
    dependencies: &toml::map::Map<String, TomlValue>,
    config: &ReleaseConfig,
    names: &mut BTreeSet<String>,
) {
    for (dependency_name, dependency) in dependencies {
        let Some(details) = dependency.as_table() else {
            continue;
        };
        if details.get("path").is_none() {
            continue;
        }
        let package_name = details
            .get("package")
            .and_then(TomlValue::as_str)
            .unwrap_or(dependency_name);
        if config.package_by_published_name(package_name).is_some() {
            names.insert(package_name.to_string());
        }
    }
}

pub fn write_verification(root: &Path, artifacts: &PackageArtifacts) -> Result<()> {
    let path = root.join(".geam/target/package/geam-verification.json");
    fs::write(
        &path,
        serde_json::to_vec_pretty(artifacts).context("could not encode verification metadata")?,
    )
    .with_context(|| format!("could not write {}", path.display()))
}

fn inspect_archive(path: &Path, package: &PackageConfig, config: &ReleaseConfig) -> Result<()> {
    let decoder = GzDecoder::new(
        File::open(path).with_context(|| format!("could not open {}", path.display()))?,
    );
    let mut archive = Archive::new(decoder);
    let mut manifest = None;
    let mut has_readme = false;
    let mut has_rust_source = false;
    for entry in archive.entries().context("could not read crate archive")? {
        let mut entry = entry.context("could not read crate archive entry")?;
        let entry_path = entry.path().context("crate archive path is invalid")?;
        if entry_path
            .file_name()
            .is_some_and(|name| name == "README.geam.md")
        {
            has_readme = true;
        }
        if entry_path
            .extension()
            .is_some_and(|extension| extension == "rs")
        {
            has_rust_source = true;
        }
        if entry_path
            .file_name()
            .is_some_and(|name| name == "Cargo.toml")
        {
            let mut source = String::new();
            entry
                .read_to_string(&mut source)
                .context("packaged Cargo.toml was not UTF-8")?;
            manifest = Some(source);
        }
    }
    ensure!(
        has_readme,
        "{} does not include its mirror README",
        package.published_name
    );
    ensure!(
        has_rust_source,
        "{} contains no Rust source",
        package.published_name
    );

    let manifest: TomlValue = toml::from_str(
        &manifest.with_context(|| format!("{} has no Cargo.toml", package.published_name))?,
    )
    .context("packaged Cargo.toml is invalid")?;
    ensure!(
        manifest["package"]["name"].as_str() == Some(&package.published_name),
        "packaged name mismatch for {}",
        package.published_name
    );
    let expected_version = config.package_version()?.to_string();
    ensure!(
        manifest["package"]["version"].as_str() == Some(expected_version.as_str()),
        "packaged version mismatch for {}",
        package.published_name
    );
    verify_packaged_dependencies(&manifest, config)?;
    Ok(())
}

fn verify_packaged_dependencies(manifest: &TomlValue, config: &ReleaseConfig) -> Result<()> {
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(dependencies) = manifest.get(section).and_then(TomlValue::as_table) {
            verify_dependency_table(dependencies, config)?;
        }
    }
    if let Some(targets) = manifest.get("target").and_then(TomlValue::as_table) {
        for target in targets.values().filter_map(TomlValue::as_table) {
            for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                if let Some(dependencies) = target.get(section).and_then(TomlValue::as_table) {
                    verify_dependency_table(dependencies, config)?;
                }
            }
        }
    }
    Ok(())
}

fn verify_dependency_table(
    dependencies: &toml::map::Map<String, TomlValue>,
    config: &ReleaseConfig,
) -> Result<()> {
    let expected_version = format!("={}", config.package_version()?);
    for (dependency_name, details) in dependencies {
        let Some(details) = details.as_table() else {
            continue;
        };
        ensure!(
            details.get("path").is_none(),
            "path dependency remains in packaged manifest"
        );
        let Some(package_name) = details.get("package").and_then(TomlValue::as_str) else {
            continue;
        };
        if config.package_by_published_name(package_name).is_some() {
            ensure!(
                details.get("version").and_then(TomlValue::as_str) == Some(&expected_version),
                "mirror dependency does not use the exact release version"
            );
        }
        if let Some(pin) = config.dependency_pin(dependency_name) {
            ensure!(
                details.get("version").and_then(TomlValue::as_str) == Some(pin.version.as_str()),
                "packaged dependency does not preserve the configured pin: {dependency_name}"
            );
        }
    }
    Ok(())
}

fn sha256(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("could not read {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn git_head(root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .context("could not read mirror HEAD")?;
    ensure!(output.status.success(), "git rev-parse HEAD failed");
    Ok(String::from_utf8(output.stdout)
        .context("git HEAD was not UTF-8")?
        .trim()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PackageConfig;
    use tempfile::TempDir;

    #[test]
    fn artifact_metadata_round_trips() {
        let artifacts = PackageArtifacts {
            upstream_tag: "v1.18.1".into(),
            upstream_commit: "a".repeat(40),
            mirror_commit: "b".repeat(40),
            package_version: "1.18.1-geam.1".into(),
            geam_commit: Some("c".repeat(40)),
            packages: vec![PackageArtifact {
                name: "geam-gleam-core".into(),
                file: ".geam/target/package/core.crate".into(),
                sha256: "d".repeat(64),
                size: 42,
            }],
        };

        let encoded = serde_json::to_string(&artifacts).unwrap();
        assert_eq!(
            serde_json::from_str::<PackageArtifacts>(&encoded).unwrap(),
            artifacts
        );
    }

    #[test]
    fn writes_local_registry_patches_for_bootstrap_packaging() {
        let root = TempDir::new().unwrap();
        let term = PackageConfig {
            path: "erlang-term-format".into(),
            source_name: "erlang-term-format".into(),
            published_name: "geam-gleam-erlang-term-format".into(),
            crate_name: "erlang_term_format".into(),
            description: "Term format".into(),
        };
        let core = PackageConfig {
            path: "compiler-core".into(),
            source_name: "gleam-core".into(),
            published_name: "geam-gleam-core".into(),
            crate_name: "gleam_core".into(),
            description: "Compiler core".into(),
        };
        fs::create_dir(root.path().join("compiler-core")).unwrap();
        fs::write(
            root.path().join("compiler-core/Cargo.toml"),
            r#"[package]
name = "geam-gleam-core"
version = "1.18.1-geam.1"

[dependencies]
term = { path = "../erlang-term-format", package = "geam-gleam-erlang-term-format", version = "=1.18.1-geam.1" }
"#,
        )
        .unwrap();
        let config = ReleaseConfig {
            upstream_repository: "gleam-lang/gleam".into(),
            upstream_tag: "v1.18.1".into(),
            upstream_commit: "a".repeat(40),
            revision: 1,
            repository: "https://github.com/panarch/gleam".into(),
            dependency_pins: vec![],
            packages: vec![term, core.clone()],
        };

        let path = write_local_patches(root.path(), root.path(), &config, &core)
            .unwrap()
            .unwrap();
        let document = fs::read_to_string(path)
            .unwrap()
            .parse::<DocumentMut>()
            .unwrap();
        assert_eq!(
            document["patch"]["crates-io"]["geam-gleam-erlang-term-format"]["path"].as_str(),
            Some(
                root.path()
                    .join("erlang-term-format")
                    .to_string_lossy()
                    .as_ref()
            )
        );
        assert!(
            document["patch"]["crates-io"]
                .get("geam-gleam-core")
                .is_none()
        );
    }
}
