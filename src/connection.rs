use serde::Deserialize;
use std::path::PathBuf;
use std::process::Command;
#[cfg(windows)]
use std::process::Stdio;
#[cfg(windows)]
use std::time::{Duration, Instant};

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    #[serde(default)]
    backend: Backend,
    codex_path: Option<PathBuf>,
    wsl_distro: Option<String>,
    wsl_user: Option<String>,
}

#[derive(Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Backend {
    #[default]
    Auto,
    Native,
    Wsl,
}

pub struct Candidate {
    pub label: String,
    native: Option<PathBuf>,
    distro: Option<String>,
    user: Option<String>,
}

impl Candidate {
    pub fn command(&self) -> Command {
        if let Some(path) = &self.native {
            let mut command = Command::new(path);
            command.args(["app-server", "--listen", "stdio://"]);
            return command;
        }
        let mut command = Command::new("wsl.exe");
        if let Some(distro) = &self.distro {
            command.args(["--distribution", distro]);
        }
        if let Some(user) = &self.user {
            command.args(["--user", user]);
        }
        // No personal username, home directory, or distro name is embedded.
        // This fixed shell program never interpolates user configuration.
        command.args(["--cd", "~", "--exec", "sh", "-lc",
            "PATH=$HOME/.local/bin:$HOME/.npm-global/bin:$HOME/.cargo/bin:$PATH; export PATH; exec codex app-server --listen stdio://"]);
        command
    }
}

fn config() -> Result<Config, String> {
    let path = match std::env::var_os("CODEX_QUOTA_CONFIG") {
        Some(path) => PathBuf::from(path),
        None => std::env::current_exe()
            .map_err(|_| "无法定位程序配置目录".to_string())?
            .with_file_name("codex-quota.json"),
    };
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|_| "codex-quota.json 配置无效，请参照示例检查字段".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(_) => Err("无法读取 codex-quota.json 配置文件".into()),
    }
}

fn native_paths(config: &Config) -> Vec<PathBuf> {
    if let Some(path) = &config.codex_path {
        return vec![path.clone()];
    }
    if let Some(path) = std::env::var_os("CODEX_QUOTA_CODEX") {
        return vec![PathBuf::from(path)];
    }
    let mut paths = Vec::new();
    let executable = if cfg!(windows) { "codex.exe" } else { "codex" };
    if let Some(search) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&search) {
            let path = directory.join(executable);
            if path.is_file() && !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let root = PathBuf::from(local)
            .join("OpenAI")
            .join("Codex")
            .join("bin");
        let mut bundled: Vec<_> = std::fs::read_dir(root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path().join("codex.exe"))
            .filter(|path| path.is_file())
            .collect();
        bundled.sort_by_key(|p| {
            std::cmp::Reverse(std::fs::metadata(p).and_then(|m| m.modified()).ok())
        });
        for path in bundled {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    #[cfg(windows)]
    if let Some(roaming) = std::env::var_os("APPDATA") {
        // npm installations use a .cmd shim; invoke the underlying executable
        // directly so paths and arguments never pass through cmd.exe.
        let root = PathBuf::from(roaming).join("npm/node_modules/@openai");
        let triple = if std::env::consts::ARCH == "aarch64" {
            "aarch64-pc-windows-msvc"
        } else {
            "x86_64-pc-windows-msvc"
        };
        for package in ["codex", "codex-win32-x64", "codex-win32-arm64"] {
            let path = root
                .join(package)
                .join("vendor")
                .join(triple)
                .join("codex/codex.exe");
            if path.is_file() && !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    #[cfg(not(windows))]
    if let Some(home) = std::env::var_os("HOME") {
        let path = PathBuf::from(home).join(".local/bin/codex");
        if path.is_file() && !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

#[cfg(any(windows, test))]
fn decode_wsl_list(bytes: &[u8]) -> Vec<String> {
    let text = if bytes.starts_with(&[0xff, 0xfe])
        || bytes.iter().skip(1).step_by(2).take(16).any(|b| *b == 0)
    {
        let units: Vec<_> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

#[cfg(windows)]
fn wsl_distros() -> Vec<String> {
    use std::os::windows::process::CommandExt;
    let Ok(mut child) = Command::new("wsl.exe")
        .args(["--list", "--quiet"])
        .creation_flags(0x08000000)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut bytes = Vec::new();
        let _ = stdout.take(65536).read_to_end(&mut bytes);
        bytes
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                return decode_wsl_list(&reader.join().unwrap_or_default())
            }
            Ok(Some(_)) | Err(_) => return Vec::new(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Vec::new();
            }
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

pub fn discover() -> Result<Vec<Candidate>, String> {
    let config = config()?;
    let mut candidates = Vec::new();
    if config.backend != Backend::Wsl {
        for path in native_paths(&config) {
            candidates.push(Candidate {
                label: if cfg!(windows) {
                    "Windows Codex"
                } else {
                    "本机 Codex"
                }
                .into(),
                native: Some(path),
                distro: None,
                user: None,
            });
        }
    }
    #[cfg(windows)]
    if config.backend != Backend::Native {
        let distros: Vec<Option<String>> = if let Some(distro) = config.wsl_distro {
            vec![Some(distro)]
        } else {
            let mut distros = vec![None]; // WSL default, without assuming its name.
            distros.extend(wsl_distros().into_iter().map(Some));
            distros
        };
        for distro in distros {
            candidates.push(Candidate {
                label: format!("WSL {}", distro.as_deref().unwrap_or("默认")),
                native: None,
                distro,
                user: config.wsl_user.clone(),
            });
        }
    }
    if candidates.is_empty() {
        Err("未找到 Codex。请安装并登录 ChatGPT，或在配置中指定 codexPath".into())
    } else {
        Ok(candidates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_names_are_not_assumed_and_utf16_is_supported() {
        let input = "\u{feff}Custom Linux\r\nDebian\r\n";
        let bytes: Vec<_> = input.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(decode_wsl_list(&bytes), vec!["Custom Linux", "Debian"]);
        assert_eq!(decode_wsl_list(b"Alpine\n"), vec!["Alpine"]);
    }

    #[test]
    fn config_rejects_typos_and_supports_explicit_selection() {
        let config: Config = serde_json::from_str(
            r#"{"backend":"wsl","wslDistro":"Custom Linux","wslUser":"someone"}"#,
        )
        .unwrap();
        assert!(config.backend == Backend::Wsl);
        assert_eq!(config.wsl_distro.as_deref(), Some("Custom Linux"));
        assert_eq!(config.wsl_user.as_deref(), Some("someone"));
        assert!(serde_json::from_str::<Config>(r#"{"wslDistribution":"oops"}"#).is_err());
        assert!(serde_json::from_str::<Config>(r#"{"backend":"unknown"}"#).is_err());
    }

    #[test]
    fn configured_names_are_separate_arguments() {
        let candidate = Candidate {
            label: String::new(),
            native: None,
            distro: Some("A distro; touch unsafe".into()),
            user: Some("user with spaces".into()),
        };
        let command = candidate.command();
        let args: Vec<_> = command
            .get_args()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[1], "A distro; touch unsafe");
        assert_eq!(args[3], "user with spaces");
        assert!(!args.last().unwrap().contains("touch unsafe"));
    }
}
