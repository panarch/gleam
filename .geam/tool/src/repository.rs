use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, bail, ensure};

use crate::config::ReleaseConfig;

const REMOVED_UPSTREAM_WORKFLOWS: [&str; 3] = [
    ".github/workflows/release.yaml",
    ".github/workflows/release-containers.yaml",
    ".github/workflows/release-nightly.yaml",
];
const GENERATED_VERSION_SOURCES: [&str; 2] = ["compiler-core/src/version.rs", "hexpm/src/lib.rs"];

pub fn apply(root: &Path) -> Result<()> {
    for workflow in REMOVED_UPSTREAM_WORKFLOWS {
        let path = root.join(workflow);
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("could not remove {}", path.display()))?;
        }
    }
    Ok(())
}

pub fn verify(root: &Path, config: &ReleaseConfig) -> Result<()> {
    let tagged_commit = git_output(
        root,
        &["rev-parse", &format!("{}^{{commit}}", config.upstream_tag)],
    )?;
    ensure!(
        tagged_commit.trim() == config.upstream_commit,
        "{} resolves to {}, expected {}",
        config.upstream_tag,
        tagged_commit.trim(),
        config.upstream_commit
    );
    ensure!(
        git_status(
            root,
            &[
                "merge-base",
                "--is-ancestor",
                &config.upstream_commit,
                "HEAD"
            ]
        )?,
        "recorded upstream commit is not an ancestor of HEAD"
    );

    verify_tracked_changes(root, config)?;
    verify_untracked_changes(root, config)?;
    for workflow in REMOVED_UPSTREAM_WORKFLOWS {
        ensure!(
            !root.join(workflow).exists(),
            "upstream release workflow remains enabled in the fork: {workflow}"
        );
    }
    Ok(())
}

fn verify_tracked_changes(root: &Path, config: &ReleaseConfig) -> Result<()> {
    let changes = git_output(root, &["diff", "--name-status", &config.upstream_tag, "--"])?;
    for line in changes.lines() {
        let mut columns = line.split('\t');
        let status = columns.next().context("git diff status is missing")?;
        let paths = columns.collect::<Vec<_>>();
        ensure!(!paths.is_empty(), "git diff path is missing");
        for path in paths {
            if path == "Cargo.lock" || path.ends_with("/Cargo.toml") || path == "Cargo.toml" {
                continue;
            }
            if is_package_readme(path, config) {
                continue;
            }
            if GENERATED_VERSION_SOURCES.contains(&path) {
                continue;
            }
            if path.starts_with(".geam/") || is_geam_workflow(path) {
                continue;
            }
            if REMOVED_UPSTREAM_WORKFLOWS.contains(&path) && status == "D" {
                continue;
            }
            bail!("change outside the packaging overlay: {status}\t{path}");
        }
    }
    Ok(())
}

fn verify_untracked_changes(root: &Path, config: &ReleaseConfig) -> Result<()> {
    let files = git_output(root, &["ls-files", "--others", "--exclude-standard"])?;
    for path in files.lines() {
        if path.starts_with(".geam/") || is_geam_workflow(path) {
            continue;
        }
        if is_package_readme(path, config) {
            continue;
        }
        bail!("untracked file outside the packaging overlay: {path}");
    }
    Ok(())
}

fn is_package_readme(path: &str, config: &ReleaseConfig) -> bool {
    config
        .packages
        .iter()
        .any(|package| path == format!("{}/README.geam.md", package.path))
}

fn is_geam_workflow(path: &str) -> bool {
    path.starts_with(".github/workflows/geam-")
        && (path.ends_with(".yml") || path.ends_with(".yaml"))
}

fn git_output(root: &Path, arguments: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .output()
        .with_context(|| format!("could not run git {}", arguments.join(" ")))?;
    ensure!(
        output.status.success(),
        "git {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8(output.stdout).context("git output was not UTF-8")
}

pub fn read_tagged_file(root: &Path, tag: &str, path: &str) -> Result<String> {
    git_output(root, &["show", &format!("{tag}:{path}")])
}

fn git_status(root: &Path, arguments: &[&str]) -> Result<bool> {
    let status = Command::new("git")
        .args(arguments)
        .current_dir(root)
        .status()
        .with_context(|| format!("could not run git {}", arguments.join(" ")))?;
    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn recognizes_only_geam_owned_workflow_names() {
        assert!(is_geam_workflow(".github/workflows/geam-sync-upstream.yml"));
        assert!(!is_geam_workflow(".github/workflows/release.yaml"));
        assert!(!is_geam_workflow(".github/workflows/not-geam-release.yml"));

        let config = ReleaseConfig {
            upstream_repository: "gleam-lang/gleam".into(),
            upstream_tag: "v1.18.1".into(),
            upstream_commit: "a".repeat(40),
            revision: 1,
            repository: "https://github.com/panarch/gleam".into(),
            dependency_pins: vec![],
            packages: vec![crate::config::PackageConfig {
                path: "compiler-core".into(),
                source_name: "gleam-core".into(),
                published_name: "geam-gleam-core".into(),
                crate_name: "gleam_core".into(),
                description: "Compiler core".into(),
            }],
        };
        assert!(is_package_readme("compiler-core/README.geam.md", &config));
        assert!(!is_package_readme("compiler-core/README.md", &config));
    }

    #[test]
    fn removes_only_upstream_release_automation() {
        let root = TempDir::new().unwrap();
        let workflows = root.path().join(".github/workflows");
        fs::create_dir_all(&workflows).unwrap();
        for workflow in REMOVED_UPSTREAM_WORKFLOWS {
            let path = root.path().join(workflow);
            fs::write(path, "upstream").unwrap();
        }
        let geam = workflows.join("geam-verify-packaging.yml");
        fs::write(&geam, "geam").unwrap();

        apply(root.path()).unwrap();

        assert!(
            REMOVED_UPSTREAM_WORKFLOWS
                .iter()
                .all(|workflow| !root.path().join(workflow).exists())
        );
        assert_eq!(fs::read_to_string(geam).unwrap(), "geam");
    }
}
