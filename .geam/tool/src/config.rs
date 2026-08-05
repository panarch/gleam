use std::{collections::HashSet, fs, path::Path};

use anyhow::{Context, Result, bail, ensure};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ReleaseConfig {
    pub upstream_repository: String,
    pub upstream_tag: String,
    pub upstream_commit: String,
    pub revision: u64,
    pub repository: String,
    pub dependency_pins: Vec<DependencyPin>,
    pub packages: Vec<PackageConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DependencyPin {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct PackageConfig {
    pub path: String,
    pub source_name: String,
    pub published_name: String,
    pub crate_name: String,
    pub description: String,
}

impl ReleaseConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let source = fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let config: Self = toml::from_str(&source)
            .with_context(|| format!("could not parse {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let source = toml::to_string_pretty(self).context("could not encode release config")?;
        fs::write(path, source).with_context(|| format!("could not write {}", path.display()))
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.upstream_repository.trim().is_empty(),
            "upstream repository is empty"
        );
        ensure!(self.revision > 0, "packaging revision must be positive");
        ensure!(
            self.upstream_commit.len() == 40
                && self.upstream_commit.chars().all(|c| c.is_ascii_hexdigit()),
            "upstream commit must be a full hexadecimal SHA"
        );
        ensure!(
            self.repository.starts_with("https://github.com/"),
            "package repository must be an HTTPS GitHub URL"
        );

        let version = self.upstream_version()?;
        ensure!(
            version.pre.is_empty() && version.build.is_empty(),
            "only stable upstream release tags are supported"
        );

        let mut paths = HashSet::new();
        let mut source_names = HashSet::new();
        let mut published_names = HashSet::new();
        let mut crate_names = HashSet::new();
        let mut dependency_names = HashSet::new();
        for pin in &self.dependency_pins {
            ensure!(!pin.name.trim().is_empty(), "dependency pin name is empty");
            ensure!(
                dependency_names.insert(&pin.name),
                "duplicate dependency pin: {}",
                pin.name
            );
            ensure!(
                pin.version.starts_with('='),
                "dependency pin is not exact: {}",
                pin.name
            );
            VersionReq::parse(&pin.version)
                .with_context(|| format!("invalid dependency pin for {}", pin.name))?;
        }
        for package in &self.packages {
            ensure!(
                Path::new(&package.path).components().count() == 1,
                "package path must be one workspace-root directory: {}",
                package.path
            );
            ensure!(paths.insert(&package.path), "duplicate package path");
            ensure!(
                source_names.insert(&package.source_name),
                "duplicate source package name"
            );
            ensure!(
                published_names.insert(&package.published_name),
                "duplicate published package name"
            );
            ensure!(
                crate_names.insert(&package.crate_name),
                "duplicate Rust crate name"
            );
            ensure!(
                package.published_name.starts_with("geam-gleam-"),
                "published package lacks geam-gleam prefix: {}",
                package.published_name
            );
            ensure!(
                !package.description.trim().is_empty(),
                "package description is empty: {}",
                package.path
            );
        }
        ensure!(self.packages.len() == 5, "expected exactly five packages");
        Ok(())
    }

    pub fn upstream_version(&self) -> Result<Version> {
        let Some(version) = self.upstream_tag.strip_prefix('v') else {
            bail!("upstream tag must start with v");
        };
        Version::parse(version).context("upstream tag is not a semantic version")
    }

    pub fn package_version(&self) -> Result<Version> {
        let upstream = self.upstream_version()?;
        Version::parse(&format!("{upstream}-geam.{}", self.revision))
            .context("could not construct package version")
    }

    pub fn release_tag(&self) -> Result<String> {
        Ok(format!(
            "geam-v{}-geam.{}",
            self.upstream_version()?,
            self.revision
        ))
    }

    pub fn package_by_path(&self, path: &Path) -> Option<&PackageConfig> {
        self.packages
            .iter()
            .find(|package| path == Path::new(&package.path))
    }

    pub fn package_by_published_name(&self, name: &str) -> Option<&PackageConfig> {
        self.packages
            .iter()
            .find(|package| package.published_name == name)
    }

    pub fn dependency_pin(&self, name: &str) -> Option<&DependencyPin> {
        self.dependency_pins.iter().find(|pin| pin.name == name)
    }

    pub fn set_release(&mut self, tag: String, commit: String, revision: u64) -> Result<()> {
        self.upstream_tag = tag;
        self.upstream_commit = commit;
        self.revision = revision;
        self.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ReleaseConfig {
        ReleaseConfig {
            upstream_repository: "gleam-lang/gleam".into(),
            upstream_tag: "v1.18.1".into(),
            upstream_commit: "4a83802ca33a8a96227a1b332768725f232f9779".into(),
            revision: 2,
            repository: "https://github.com/panarch/gleam".into(),
            dependency_pins: vec![DependencyPin {
                name: "ecow".into(),
                version: "=0.2.6".into(),
            }],
            packages: ["term-format", "generation", "arena", "hexpm", "core"]
                .into_iter()
                .map(|name| PackageConfig {
                    path: name.into(),
                    source_name: name.into(),
                    published_name: format!("geam-gleam-{name}"),
                    crate_name: name.replace('-', "_"),
                    description: format!("{name} package"),
                })
                .collect(),
        }
    }

    #[test]
    fn derives_upstream_package_and_release_versions() {
        let config = config();

        assert_eq!(config.upstream_version().unwrap(), Version::new(1, 18, 1));
        assert_eq!(
            config.package_version().unwrap().to_string(),
            "1.18.1-geam.2"
        );
        assert_eq!(config.release_tag().unwrap(), "geam-v1.18.1-geam.2");
    }

    #[test]
    fn rejects_non_release_and_duplicate_package_configuration() {
        let mut invalid_tag = config();
        invalid_tag.upstream_tag = "main".into();
        assert_eq!(
            invalid_tag.validate().unwrap_err().to_string(),
            "upstream tag must start with v"
        );

        let mut duplicate_package = config();
        duplicate_package.packages[1].published_name =
            duplicate_package.packages[0].published_name.clone();
        assert_eq!(
            duplicate_package.validate().unwrap_err().to_string(),
            "duplicate published package name"
        );

        let mut broad_dependency = config();
        broad_dependency.dependency_pins[0].version = "0.2".into();
        assert_eq!(
            broad_dependency.validate().unwrap_err().to_string(),
            "dependency pin is not exact: ecow"
        );
    }
}
