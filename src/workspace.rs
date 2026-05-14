use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

pub fn resolve_cwd(repo_root: &Path, cwd: Option<&str>) -> Result<PathBuf> {
    resolve_child(repo_root, ".", cwd.unwrap_or("."))
}

pub fn resolve_child(repo_root: &Path, cwd: &str, input: &str) -> Result<PathBuf> {
    let input = input.trim();
    let input = if input.is_empty() { "." } else { input };
    if Path::new(input).is_absolute() {
        bail!("use a relative path inside the selected repo");
    }

    let root = canonical_root(repo_root)?;
    let base = if cwd.trim().is_empty() || cwd == "." {
        root.clone()
    } else {
        root.join(cwd)
    };
    let candidate = base.join(input);
    let resolved = candidate
        .canonicalize()
        .with_context(|| format!("resolving {}", candidate.display()))?;
    if !resolved.starts_with(&root) {
        bail!("path is outside the selected repo");
    }
    Ok(resolved)
}

pub fn relative_cwd(repo_root: &Path, path: &Path) -> Result<String> {
    let root = canonical_root(repo_root)?;
    let path = path
        .canonicalize()
        .with_context(|| format!("resolving {}", path.display()))?;
    if !path.starts_with(&root) {
        bail!("path is outside the selected repo");
    }
    let relative = path
        .strip_prefix(&root)
        .with_context(|| format!("building path under {}", root.display()))?;
    Ok(path_to_slash(relative))
}

pub fn display_cwd(cwd: &str) -> String {
    if cwd.trim().is_empty() || cwd == "." {
        "/".to_string()
    } else {
        format!("/{}", cwd.trim_matches('/'))
    }
}

fn canonical_root(repo_root: &Path) -> Result<PathBuf> {
    let root = repo_root
        .canonicalize()
        .with_context(|| format!("resolving repo {}", repo_root.display()))?;
    if !root.is_dir() {
        bail!("repo path is not a directory: {}", root.display());
    }
    Ok(root)
}

fn path_to_slash(path: &Path) -> String {
    let parts = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy().to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_paths_inside_repo() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        let path = resolve_child(temp.path(), ".", "src").unwrap();
        assert_eq!(relative_cwd(temp.path(), &path).unwrap(), "src");
        assert!(resolve_child(temp.path(), ".", "../").is_err());
    }

    #[test]
    fn displays_root_as_slash() {
        assert_eq!(display_cwd("."), "/");
        assert_eq!(display_cwd("src/bin"), "/src/bin");
    }
}
