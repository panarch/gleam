use std::{fs, path::Path};

use anyhow::{Context, Result, ensure};
use toml::Value;

use crate::{config::ReleaseConfig, repository};

pub fn verify(root: &Path, config: &ReleaseConfig) -> Result<()> {
    let upstream = repository::read_tagged_file(root, &config.upstream_tag, "Cargo.lock")?;
    let mut expected = transform(&upstream, config)?;
    let actual =
        fs::read_to_string(root.join("Cargo.lock")).context("could not read Cargo.lock")?;
    let mut actual: Value = toml::from_str(&actual).context("could not parse Cargo.lock")?;
    normalize(&mut expected)?;
    normalize(&mut actual)?;
    ensure!(
        actual == expected,
        "Cargo.lock differs from the canonical upstream package-identity transform"
    );
    Ok(())
}

fn transform(source: &str, config: &ReleaseConfig) -> Result<Value> {
    let mut lock: Value = toml::from_str(source).context("could not parse upstream Cargo.lock")?;
    let packages = lock
        .get_mut("package")
        .and_then(Value::as_array_mut)
        .context("upstream Cargo.lock has no package array")?;
    let version = config.package_version()?.to_string();

    for package in packages {
        let Some(table) = package.as_table_mut() else {
            continue;
        };
        if table.get("source").is_none()
            && let Some(name) = table.get("name").and_then(Value::as_str)
            && let Some(mirrored) = config
                .packages
                .iter()
                .find(|package| package.source_name == name)
        {
            table.insert(
                "name".into(),
                Value::String(mirrored.published_name.clone()),
            );
            table.insert("version".into(), Value::String(version.clone()));
        }

        let Some(dependencies) = table.get_mut("dependencies").and_then(Value::as_array_mut) else {
            continue;
        };
        for dependency in dependencies {
            let Some(name) = dependency.as_str() else {
                continue;
            };
            if let Some(mirrored) = config
                .packages
                .iter()
                .find(|package| package.source_name == name)
            {
                *dependency = Value::String(mirrored.published_name.clone());
            }
        }
    }
    Ok(lock)
}

fn normalize(lock: &mut Value) -> Result<()> {
    let packages = lock
        .get_mut("package")
        .and_then(Value::as_array_mut)
        .context("Cargo.lock has no package array")?;

    for package in packages.iter_mut() {
        let Some(dependencies) = package
            .get_mut("dependencies")
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        dependencies.sort_by(|left, right| left.as_str().cmp(&right.as_str()));
    }
    packages.sort_by(|left, right| package_key(left).cmp(&package_key(right)));
    Ok(())
}

fn package_key(package: &Value) -> (Option<&str>, Option<&str>, Option<&str>) {
    (
        package.get("name").and_then(Value::as_str),
        package.get("version").and_then(Value::as_str),
        package.get("source").and_then(Value::as_str),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PackageConfig;

    #[test]
    fn changes_only_local_package_identities_and_references() {
        let config = ReleaseConfig {
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
        };
        let source = r#"version = 4

[[package]]
name = "geam"
version = "0.1.0"
dependencies = ["gleam-core"]

[[package]]
name = "gleam-core"
version = "1.18.1"

[[package]]
name = "gleam-core"
version = "9.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

        let transformed = transform(source, &config).unwrap();
        let packages = transformed["package"].as_array().unwrap();
        assert_eq!(
            packages[0]["dependencies"].as_array().unwrap(),
            &[Value::String("geam-gleam-core".into())]
        );
        assert_eq!(packages[1]["name"].as_str(), Some("geam-gleam-core"));
        assert_eq!(packages[1]["version"].as_str(), Some("1.18.1-geam.1"));
        assert_eq!(packages[2]["name"].as_str(), Some("gleam-core"));
        assert_eq!(packages[2]["version"].as_str(), Some("9.0.0"));
    }

    #[test]
    fn normalization_ignores_cargo_package_and_dependency_order() {
        let mut left: Value = toml::from_str(
            r#"version = 4

[[package]]
name = "z"
version = "1"
dependencies = ["b", "a"]

[[package]]
name = "a"
version = "1"
"#,
        )
        .unwrap();
        let mut right: Value = toml::from_str(
            r#"version = 4

[[package]]
name = "a"
version = "1"

[[package]]
name = "z"
version = "1"
dependencies = ["a", "b"]
"#,
        )
        .unwrap();

        normalize(&mut left).unwrap();
        normalize(&mut right).unwrap();

        assert_eq!(left, right);
    }
}
