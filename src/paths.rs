use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "agentnoise";
pub const INSTANCES_DIR: &str = "instances";

pub fn default_data_dir() -> PathBuf {
    dirs::data_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Library/Application Support")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join(APP_NAME)
}

pub fn default_log_dir() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join("Library/Logs").join(APP_NAME))
        .unwrap_or_else(|| default_data_dir().join("logs"))
}

pub fn default_config_path() -> PathBuf {
    default_data_dir().join("config.toml")
}

pub fn normalize_instance_name(name: &str) -> Option<String> {
    let mut output = String::new();
    let mut last_dash = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            output.push('-');
            last_dash = true;
        }
    }
    let output = output.trim_matches('-').to_string();
    (!output.is_empty()).then_some(output)
}

pub fn instance_root(name: &str) -> PathBuf {
    default_data_dir().join(INSTANCES_DIR).join(name)
}

pub fn instance_config_path(name: &str) -> PathBuf {
    instance_root(name).join("config.toml")
}

pub fn instance_data_dir(name: &str) -> PathBuf {
    instance_root(name).join("data")
}

pub fn instance_log_dir(name: &str) -> PathBuf {
    default_log_dir().join(INSTANCES_DIR).join(name)
}

pub fn instance_name_from_config_path(path: &Path) -> Option<String> {
    let instances_root = default_data_dir().join(INSTANCES_DIR);
    let relative = path.strip_prefix(instances_root).ok()?;
    let mut components = relative.components();
    let name = components.next()?.as_os_str().to_str()?;
    let config = components.next()?.as_os_str().to_str()?;
    if config == "config.toml" && components.next().is_none() {
        Some(name.to_string())
    } else {
        None
    }
}

pub fn default_service_path() -> String {
    [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/bin",
        "/usr/sbin",
        "/sbin",
    ]
    .join(":")
}

pub fn managed_whitenoise_root() -> PathBuf {
    default_data_dir().join("whitenoise-cli")
}

pub fn managed_whitenoise_bin_dir() -> PathBuf {
    managed_whitenoise_root().join("bin")
}

pub fn managed_wn_path() -> PathBuf {
    managed_whitenoise_bin_dir().join("wn")
}

pub fn managed_wnd_path() -> PathBuf {
    managed_whitenoise_bin_dir().join("wnd")
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(path));
    }

    if let Some(rest) = path.strip_prefix("~/") {
        return dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(path));
    }

    PathBuf::from(path)
}

pub fn executable_next_to_agentnoise(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.join(name);
    path.is_file().then_some(path)
}

pub fn find_on_path(command: &str) -> Option<PathBuf> {
    let command_path = PathBuf::from(command);
    if command_path.components().count() > 1 {
        return command_path.is_file().then_some(command_path);
    }

    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(command))
        .find(|candidate| candidate.is_file())
}

pub fn local_checkout_whitenoise_bin(name: &str) -> Option<PathBuf> {
    let path = std::env::current_dir()
        .ok()?
        .join(".local-whitenoise/bin")
        .join(name);
    path.is_file().then_some(path)
}

pub fn is_gui_backed_workspace_path(path: &Path) -> bool {
    let text = path.to_string_lossy();
    text.contains("/Library/Mobile Documents/") || text.contains("/CloudDocs/")
}
