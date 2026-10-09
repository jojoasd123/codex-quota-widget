#![cfg_attr(windows, windows_subsystem = "windows")]

mod connection;
mod quota;
#[cfg(windows)]
mod ui;

fn main() {
    if let Some(path) = std::env::args()
        .nth(1)
        .filter(|s| s == "--check")
        .and_then(|_| std::env::args().nth(2))
    {
        let result = match quota::fetch() {
            Ok(snapshot) => {
                serde_json::json!({"ok":true,"source":snapshot.source,"windows":snapshot.windows.iter().map(|w| serde_json::json!({"bucket":w.bucket,"remainingPercent":w.remaining,"windowDurationMins":w.minutes})).collect::<Vec<_>>() })
            }
            Err(error) => serde_json::json!({"ok":false,"error":error}),
        };
        if std::fs::write(path, result.to_string()).is_err() || result["ok"] != true {
            std::process::exit(1);
        }
        return;
    }
    #[cfg(windows)]
    ui::run();
    #[cfg(not(windows))]
    match quota::fetch() {
        Ok(snapshot) => println!("{snapshot:#?}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
