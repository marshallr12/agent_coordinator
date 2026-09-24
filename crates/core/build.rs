use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=COORDINATOR_BUILD_COMMIT");
    let manifest =
        Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir")).to_path_buf();
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .expect("workspace root");
    rerun_for_source_tree(root);
    println!("cargo:rerun-if-env-changed=COORDINATOR_BUILD_DIRTY");
    println!("cargo:rerun-if-env-changed=COORDINATOR_SOURCE_REPOSITORY");
    let configured_commit = std::env::var("COORDINATOR_BUILD_COMMIT").ok();
    let commit = configured_commit
        .clone()
        .filter(|value| valid_commit(value))
        .or_else(|| {
            Command::new("git")
                .args(["-C", root.to_str()?, "rev-parse", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|value| value.trim().to_owned())
                .filter(|value| valid_commit(value))
        })
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=COORDINATOR_SOURCE_COMMIT={commit}");
    let repository = std::env::var("COORDINATOR_SOURCE_REPOSITORY")
        .ok()
        .filter(|value| valid_repository(value))
        .or_else(|| git_repository(root))
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=COORDINATOR_SOURCE_REPOSITORY={repository}");
    let dirty = std::env::var("COORDINATOR_BUILD_DIRTY")
        .ok()
        .unwrap_or_else(|| {
            if configured_commit.is_some() {
                return "false".to_owned();
            }
            Command::new("git")
                .args([
                    "-C",
                    root.to_str().unwrap_or_default(),
                    "status",
                    "--porcelain",
                ])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| (!output.stdout.is_empty()).to_string())
                .unwrap_or_else(|| "true".to_owned())
        });
    println!("cargo:rustc-env=COORDINATOR_BUILD_DIRTY={dirty}");
    println!(
        "cargo:rustc-env=COORDINATOR_TARGET_OS={}",
        std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_else(|_| "unknown".into())
    );
    println!(
        "cargo:rustc-env=COORDINATOR_TARGET_ARCH={}",
        std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "unknown".into())
    );
}

fn rerun_for_source_tree(root: &Path) {
    let Some(root) = root.to_str() else {
        return;
    };
    if let Ok(output) = Command::new("git")
        .args(["-C", root, "ls-files", "-z"])
        .output()
        && output.status.success()
    {
        for path in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            if let Ok(path) = std::str::from_utf8(path) {
                println!("cargo:rerun-if-changed={root}/{path}");
            }
        }
    }
    if let Ok(output) = Command::new("git")
        .args(["-C", root, "rev-parse", "--git-path", "HEAD"])
        .output()
        && output.status.success()
        && let Ok(path) = String::from_utf8(output.stdout)
    {
        let path = Path::new(path.trim());
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            Path::new(root).join(path)
        };
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn valid_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_repository(value: &str) -> bool {
    value.starts_with("https://")
        && !value.contains('@')
        && !value.contains('?')
        && !value.contains('#')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b":/-._".contains(&byte))
}

fn git_repository(root: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", root.to_str()?, "config", "--get", "remote.origin.url"])
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let value = String::from_utf8(output.stdout).ok()?;
    let value = value.trim();
    let https = if let Some(value) = value.strip_prefix("https://") {
        format!(
            "https://{}",
            value.rsplit_once('@').map_or(value, |(_, path)| path)
        )
    } else {
        let value = value.strip_prefix("git@github.com:")?;
        format!("https://github.com/{value}")
    };
    let normalized = https.strip_suffix(".git").unwrap_or(&https);
    valid_repository(normalized).then(|| normalized.to_owned())
}
