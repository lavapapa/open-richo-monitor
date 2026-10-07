use std::path::{Path, PathBuf};

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TARGET: &str = "aarch64-apple-darwin";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const TARGET: &str = "x86_64-apple-darwin";
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const TARGET: &str = "x86_64-pc-windows-msvc";

pub(crate) fn current() -> std::io::Result<PathBuf> {
    #[cfg(debug_assertions)]
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    #[cfg(not(debug_assertions))]
    let directory = std::env::current_exe()?
        .parent()
        .ok_or_else(|| std::io::Error::other("无法定位通知模块"))?
        .to_path_buf();
    Ok(runtime_path(&directory, TARGET, cfg!(debug_assertions)))
}

pub(crate) fn arguments(resources: &Path) -> std::io::Result<Vec<String>> {
    #[cfg(debug_assertions)]
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join("notification-runtime.mjs");
    #[cfg(not(debug_assertions))]
    let script = resources.join("notification-runtime.mjs");
    #[cfg(debug_assertions)]
    let _ = resources;
    let arguments = vec![
        "--no-env-file".into(),
        "--no-install".into(),
        script.to_string_lossy().into_owned(),
    ];
    #[cfg(target_os = "windows")]
    let arguments = arguments
        .into_iter()
        .chain([
            "--system-proxy-helper".into(),
            std::env::current_exe()?.to_string_lossy().into_owned(),
        ])
        .collect();
    Ok(arguments)
}

fn runtime_path(directory: &Path, target: &str, debug: bool) -> PathBuf {
    let extension = if target == "x86_64-pc-windows-msvc" {
        ".exe"
    } else {
        ""
    };
    if debug {
        directory
            .join("binaries")
            .join(format!("notification-runtime-{target}{extension}"))
    } else {
        directory.join(format!("notification-runtime{extension}"))
    }
}

#[cfg(test)]
mod tests {
    use super::runtime_path;
    use std::path::Path;

    #[test]
    fn debug_paths_preserve_chinese_and_spaces_and_use_target_suffix() {
        let directory = Path::new("理光 项目");
        for (target, filename) in [
            (
                "aarch64-apple-darwin",
                "notification-runtime-aarch64-apple-darwin",
            ),
            (
                "x86_64-apple-darwin",
                "notification-runtime-x86_64-apple-darwin",
            ),
            (
                "x86_64-pc-windows-msvc",
                "notification-runtime-x86_64-pc-windows-msvc.exe",
            ),
        ] {
            assert_eq!(
                runtime_path(directory, target, true),
                directory.join("binaries").join(filename)
            );
        }
    }

    #[test]
    fn release_paths_use_neighboring_runtime_with_platform_extension() {
        let directory = Path::new("理光 安装");
        assert_eq!(
            runtime_path(directory, "aarch64-apple-darwin", false),
            directory.join("notification-runtime")
        );
        assert_eq!(
            runtime_path(directory, "x86_64-pc-windows-msvc", false),
            directory.join("notification-runtime.exe")
        );
    }
}
