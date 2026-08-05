use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use toml_edit::{Array, DocumentMut, Item, Table, TableLike, value};
use walkdir::WalkDir;

use crate::config::ReleaseConfig;

const DEPENDENCY_SECTIONS: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];
const PACKAGE_README: &str = "README.geam.md";

pub fn apply(root: &Path, config: &ReleaseConfig) -> Result<()> {
    for relative_path in upstream_manifest_paths(root, &config.upstream_tag)? {
        let manifest_path = root.join(&relative_path);
        let source = git_show(root, &config.upstream_tag, &relative_path)?;
        let transformed = transform(root, &manifest_path, &source, config)?;
        fs::write(&manifest_path, transformed)
            .with_context(|| format!("could not write {}", manifest_path.display()))?;
    }
    apply_package_readmes(root, config)?;
    Ok(())
}

pub fn verify(root: &Path, config: &ReleaseConfig) -> Result<()> {
    let upstream_manifests = upstream_manifest_paths(root, &config.upstream_tag)?;
    let current_manifests = workspace_manifests(root)?
        .into_iter()
        .map(|path| {
            path.strip_prefix(root)
                .expect("workspace manifest is under root")
                .to_path_buf()
        })
        .filter(|path| path != Path::new(".geam/tool/Cargo.toml"))
        .collect::<BTreeSet<_>>();
    ensure!(
        current_manifests == upstream_manifests,
        "workspace Cargo.toml inventory differs from upstream"
    );

    for relative_path in upstream_manifests {
        let upstream = git_show(root, &config.upstream_tag, &relative_path)?;
        let expected = transform(root, &root.join(&relative_path), &upstream, config)?;
        let actual = fs::read_to_string(root.join(&relative_path))
            .with_context(|| format!("could not read {}", relative_path.display()))?;
        if actual != expected {
            bail!(
                "{} is not the canonical packaging transform of {}",
                relative_path.display(),
                config.upstream_tag
            );
        }
    }
    verify_package_readmes(root, config)?;

    Ok(())
}

fn transform(
    root: &Path,
    manifest_path: &Path,
    source: &str,
    config: &ReleaseConfig,
) -> Result<String> {
    let mut document = source.parse::<DocumentMut>().with_context(|| {
        format!(
            "could not parse manifest {}",
            manifest_path
                .strip_prefix(root)
                .unwrap_or(manifest_path)
                .display()
        )
    })?;
    let package_path = manifest_path
        .parent()
        .and_then(|path| path.strip_prefix(root).ok())
        .unwrap_or_else(|| Path::new(""));
    let package = config.package_by_path(package_path);

    if let Some(package) = package {
        document["package"]["name"] = value(&package.published_name);
        document["package"]["version"] = value(config.package_version()?.to_string());
        document["package"]["repository"] = value(&config.repository);
        document["package"]["description"] = value(&package.description);
        document["package"]["readme"] = value(PACKAGE_README);

        let mut registries = Array::new();
        registries.push("crates-io");
        document["package"]["publish"] = value(registries);

        if document.get("lib").is_none() {
            document["lib"] = Item::Table(Table::new());
        }
        document["lib"]["name"] = value(&package.crate_name);
    }

    update_dependencies(
        root,
        manifest_path,
        package.is_some(),
        &mut document,
        config,
    )?;
    update_workspace_dependency_pins(&mut document, config);
    Ok(document.to_string())
}

fn update_workspace_dependency_pins(document: &mut DocumentMut, config: &ReleaseConfig) {
    let Some(dependencies) = document
        .get_mut("workspace")
        .and_then(Item::as_table_like_mut)
        .and_then(|workspace| workspace.get_mut("dependencies"))
        .and_then(Item::as_table_like_mut)
    else {
        return;
    };
    for pin in &config.dependency_pins {
        let Some(dependency) = dependencies.get_mut(&pin.name) else {
            continue;
        };
        if dependency.is_str() {
            *dependency = value(&pin.version);
            continue;
        }
        if let Some(details) = dependency.as_table_like_mut() {
            details.insert("version", value(&pin.version));
        }
        if let Some(inline_table) = dependency.as_inline_table_mut() {
            inline_table.fmt();
        }
    }
}

fn apply_package_readmes(root: &Path, config: &ReleaseConfig) -> Result<()> {
    let source = fs::read(root.join(".geam/CRATE_README.md"))
        .context("could not read canonical package README")?;
    for package in &config.packages {
        let path = root.join(&package.path).join(PACKAGE_README);
        fs::write(&path, &source).with_context(|| format!("could not write {}", path.display()))?;
    }
    Ok(())
}

fn verify_package_readmes(root: &Path, config: &ReleaseConfig) -> Result<()> {
    let expected = fs::read(root.join(".geam/CRATE_README.md"))
        .context("could not read canonical package README")?;
    for package in &config.packages {
        let path = root.join(&package.path).join(PACKAGE_README);
        let actual =
            fs::read(&path).with_context(|| format!("could not read {}", path.display()))?;
        ensure!(
            actual == expected,
            "{} is not the canonical package README",
            path.display()
        );
    }
    Ok(())
}

fn update_dependencies(
    root: &Path,
    manifest_path: &Path,
    published_owner: bool,
    document: &mut DocumentMut,
    config: &ReleaseConfig,
) -> Result<()> {
    for section in DEPENDENCY_SECTIONS {
        if let Some(table) = document.get_mut(section).and_then(Item::as_table_like_mut) {
            update_dependency_table(root, manifest_path, published_owner, table, config)?;
        }
    }

    let Some(targets) = document.get_mut("target").and_then(Item::as_table_like_mut) else {
        return Ok(());
    };
    for (_, target) in targets.iter_mut() {
        let Some(target) = target.as_table_like_mut() else {
            continue;
        };
        for section in DEPENDENCY_SECTIONS {
            if let Some(table) = target.get_mut(section).and_then(Item::as_table_like_mut) {
                update_dependency_table(root, manifest_path, published_owner, table, config)?;
            }
        }
    }
    Ok(())
}

fn update_dependency_table(
    root: &Path,
    manifest_path: &Path,
    published_owner: bool,
    dependencies: &mut dyn TableLike,
    config: &ReleaseConfig,
) -> Result<()> {
    for (dependency_name, dependency) in dependencies.iter_mut() {
        let pin = published_owner
            .then(|| config.dependency_pin(dependency_name.get()))
            .flatten();
        if let Some(pin) = pin
            && dependency.is_str()
        {
            *dependency = value(&pin.version);
            continue;
        }
        let Some(details) = dependency.as_table_like_mut() else {
            continue;
        };
        if let Some(path) = details.get("path").and_then(Item::as_str) {
            let target = normalize_path(
                &manifest_path
                    .parent()
                    .context("manifest has no parent")?
                    .join(path),
            );
            if let Ok(target) = target.strip_prefix(root)
                && let Some(package) = config.package_by_path(target)
            {
                details.insert("package", value(&package.published_name));
                if published_owner {
                    details.insert("version", value(format!("={}", config.package_version()?)));
                }
            }
        }
        let inherits_workspace = details.get("workspace").and_then(Item::as_bool) == Some(true);
        if let Some(pin) = pin
            && !inherits_workspace
        {
            details.insert("version", value(&pin.version));
        }
        if let Some(inline_table) = dependency.as_inline_table_mut() {
            inline_table.fmt();
        }
    }
    Ok(())
}

fn workspace_manifests(root: &Path) -> Result<Vec<PathBuf>> {
    let mut manifests = Vec::new();
    for entry in WalkDir::new(root).into_iter().filter_entry(|entry| {
        let name = entry.file_name().to_string_lossy();
        !matches!(name.as_ref(), ".git" | "target")
            && !entry.path().starts_with(root.join(".geam/tool/target"))
    }) {
        let entry = entry.context("could not walk workspace")?;
        if entry.file_type().is_file() && entry.file_name() == "Cargo.toml" {
            manifests.push(entry.into_path());
        }
    }
    manifests.sort();
    Ok(manifests)
}

fn upstream_manifest_paths(root: &Path, tag: &str) -> Result<BTreeSet<PathBuf>> {
    let output = Command::new("git")
        .args(["ls-tree", "-r", "--name-only", tag])
        .current_dir(root)
        .output()
        .context("could not list upstream tree")?;
    ensure!(output.status.success(), "git ls-tree failed");
    Ok(String::from_utf8(output.stdout)
        .context("git ls-tree output was not UTF-8")?
        .lines()
        .map(PathBuf::from)
        .filter(|path| path.file_name().is_some_and(|name| name == "Cargo.toml"))
        .collect())
}

fn git_show(root: &Path, tag: &str, path: &Path) -> Result<String> {
    let object = format!("{tag}:{}", path.display());
    let output = Command::new("git")
        .args(["show", &object])
        .current_dir(root)
        .output()
        .with_context(|| format!("could not read {object}"))?;
    ensure!(output.status.success(), "git show failed for {object}");
    String::from_utf8(output.stdout).with_context(|| format!("{object} was not UTF-8"))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DependencyPin, PackageConfig};

    fn config() -> ReleaseConfig {
        ReleaseConfig {
            upstream_repository: "gleam-lang/gleam".into(),
            upstream_tag: "v1.18.1".into(),
            upstream_commit: "4a83802ca33a8a96227a1b332768725f232f9779".into(),
            revision: 1,
            repository: "https://github.com/panarch/gleam".into(),
            dependency_pins: vec![DependencyPin {
                name: "ecow".into(),
                version: "=0.2.6".into(),
            }],
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
    fn transforms_package_identity_and_published_path_dependencies() {
        let root = Path::new("/workspace");
        let source = r#"[package]
name = "gleam-core"
version = "1.18.1"

[dependencies]
core = { path = "../compiler-core" }
ecow = { workspace = true, features = ["serde"] }
"#;

        let transformed = transform(
            root,
            Path::new("/workspace/compiler-core/Cargo.toml"),
            source,
            &config(),
        )
        .unwrap();
        let document = transformed.parse::<DocumentMut>().unwrap();

        assert_eq!(
            document["package"]["name"].as_str(),
            Some("geam-gleam-core")
        );
        assert_eq!(
            document["package"]["version"].as_str(),
            Some("1.18.1-geam.1")
        );
        assert_eq!(document["lib"]["name"].as_str(), Some("gleam_core"));
        assert!(document["dependencies"]["ecow"].get("version").is_none());
        assert_eq!(
            document["dependencies"]["core"]["package"].as_str(),
            Some("geam-gleam-core")
        );
        assert_eq!(
            document["dependencies"]["core"]["version"].as_str(),
            Some("=1.18.1-geam.1")
        );
    }

    #[test]
    fn pins_workspace_dependencies_at_their_inheritance_owner() {
        let transformed = transform(
            Path::new("/workspace"),
            Path::new("/workspace/Cargo.toml"),
            "[workspace]\n[workspace.dependencies]\necow = \"0\"\n",
            &config(),
        )
        .unwrap();
        let document = transformed.parse::<DocumentMut>().unwrap();

        assert_eq!(
            document["workspace"]["dependencies"]["ecow"].as_str(),
            Some("=0.2.6")
        );
    }

    #[test]
    fn normalizes_parent_components_without_touching_the_filesystem() {
        assert_eq!(
            normalize_path(Path::new("/workspace/compiler-core/../hexpm")),
            Path::new("/workspace/hexpm")
        );
    }
}
