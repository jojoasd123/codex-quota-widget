use crate::connection;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct Window {
    pub bucket: String,
    pub name: String,
    pub minutes: Option<i64>,
    pub remaining: i64,
    pub resets_at: Option<i64>,
}

impl Window {
    pub fn label(&self) -> String {
        match self.minutes {
            Some(10080) => "周额度".into(),
            Some(300) => "5 小时额度".into(),
            Some(m) if m > 0 && m % 1440 == 0 => format!("{} 天额度", m / 1440),
            Some(m) if m > 0 && m % 60 == 0 => format!("{} 小时额度", m / 60),
            Some(m) if m > 0 => format!("{m} 分钟额度"),
            _ => "当前额度".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Reset {
    pub expires_at: Option<i64>,
    pub never_expires: bool,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub windows: Vec<Window>,
    pub reset_count: Option<u64>,
    pub resets: Option<Vec<Reset>>,
    pub captured_at: i64,
    pub source: String,
}

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn countdown(timestamp: i64, current: i64) -> String {
    let seconds = timestamp.saturating_sub(current);
    if seconds <= 0 {
        return "等待同步".into();
    }
    let minutes = seconds.saturating_add(59) / 60;
    if minutes >= 1440 {
        format!("{} 天 {} 小时", minutes / 1440, minutes % 1440 / 60)
    } else if minutes >= 60 {
        format!("{} 小时 {} 分", minutes / 60, minutes % 60)
    } else {
        format!("{minutes} 分钟")
    }
}

pub fn parse(value: &Value) -> Result<Snapshot, String> {
    if !value.is_object() || !value.get("rateLimits").is_some_and(Value::is_object) {
        return Err("额度返回格式有变化，请更新 Codex 后重试".into());
    }
    let fallback;
    let buckets = match value["rateLimitsByLimitId"]
        .as_object()
        .filter(|v| !v.is_empty())
    {
        Some(buckets) => buckets,
        None => {
            fallback = serde_json::Map::from_iter([("codex".into(), value["rateLimits"].clone())]);
            &fallback
        }
    };
    let mut windows = Vec::new();
    for (bucket, data) in buckets {
        for key in ["primary", "secondary"] {
            let window = &data[key];
            let Some(used) = window["usedPercent"].as_f64().filter(|v| v.is_finite()) else {
                continue;
            };
            windows.push(Window {
                bucket: bucket.clone(),
                name: data["limitName"].as_str().unwrap_or(bucket).to_string(),
                minutes: window["windowDurationMins"].as_i64(),
                remaining: (100.0 - used.clamp(0.0, 100.0)).floor() as i64,
                resets_at: window["resetsAt"].as_i64(),
            });
        }
    }
    windows.sort_by_key(|w| w.bucket != "codex");
    let summary = &value["rateLimitResetCredits"];
    let resets = summary["credits"].as_array().map(|rows| {
        let mut rows: Vec<_> = rows
            .iter()
            .filter(|r| r["status"] == "available")
            .map(|r| Reset {
                expires_at: r["expiresAt"].as_i64(),
                never_expires: r.get("expiresAt").is_some_and(Value::is_null),
            })
            .collect();
        rows.sort_by_key(|r| r.expires_at.unwrap_or(i64::MAX));
        rows
    });
    if windows.is_empty() {
        return Err("账号暂未返回额度窗口，请检查 ChatGPT 登录和套餐".into());
    }
    Ok(Snapshot {
        windows,
        reset_count: summary["availableCount"].as_u64(),
        resets,
        captured_at: now(),
        source: String::new(),
    })
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        self.0.stdin.take(); // EOF lets the WSL Codex process exit as well.
        for _ in 0..60 {
            if !matches!(self.0.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn response(receiver: &Receiver<Value>, id: u64) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(25);
    loop {
        let value = receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|_| "连接超时或已断开；请检查网络和 Codex 登录状态".to_string())?;
        if value["id"].as_u64() != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            let message = error["message"].as_str().unwrap_or("").to_lowercase();
            // Only display our own messages; raw provider errors may contain credentials.
            return Err(if ["auth", "login", "401", "api key", "chatgpt"]
                .iter()
                .any(|s| message.contains(s))
            {
                "请在所选 Codex 中登录 ChatGPT 账号"
            } else {
                "暂时无法读取额度，稍后会自动重试"
            }
            .into());
        }
        return value
            .get("result")
            .cloned()
            .ok_or_else(|| "Codex 返回了无法识别的响应".into());
    }
}

pub fn fetch() -> Result<Snapshot, String> {
    let candidates = connection::discover()?;
    let mut errors = Vec::new();
    for candidate in candidates {
        match fetch_from(candidate.command()) {
            Ok(mut snapshot) => {
                snapshot.source = candidate.label;
                return Ok(snapshot);
            }
            Err(error) => errors.push(format!("{}：{}", candidate.label, error)),
        }
    }
    Err(errors.join("；"))
}

fn fetch_from(mut command: Command) -> Result<Snapshot, String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut server = Server(
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "无法启动 Codex；请检查安装或连接配置".to_string())?,
    );
    let stdout = server.0.stdout.take().ok_or("无法读取 Codex 输出")?;
    let (tx, rx) = mpsc::sync_channel(16);
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if tx.send(value).is_err() {
                    break;
                }
            }
        }
    });
    let input = server.0.stdin.as_mut().ok_or("无法连接 Codex 输入")?;
    let initialize = json!({"id": 1, "method": "initialize", "params": {
        "clientInfo": {"name": "codex_quota_widget", "title": "Codex Quota Widget", "version": env!("CARGO_PKG_VERSION")}
    }});
    writeln!(input, "{initialize}")
        .and_then(|_| input.flush())
        .map_err(|_| "Codex 连接已断开")?;
    response(&rx, 1)?;
    writeln!(input, "{}", json!({"method": "initialized"}))
        .and_then(|_| {
            writeln!(
                input,
                "{}",
                json!({"id": 2, "method": "account/rateLimits/read"})
            )
        })
        .and_then(|_| input.flush())
        .map_err(|_| "Codex 连接已断开")?;
    parse(&response(&rx, 2)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_contract_and_reset_edge_cases() {
        let mut value = json!({"rateLimits": {"primary": {"usedPercent": 61,
        "windowDurationMins": 10080, "resetsAt": 2000}},
        "rateLimitResetCredits": {"availableCount": 4, "credits": [
            {"status":"available", "expiresAt":3000},
            {"status":"redeemed", "expiresAt":1000},
            {"status":"available", "expiresAt":null},
            {"status":"available"},
            {"status":"available", "expiresAt":2000}
        ]}});
        let snapshot = parse(&value).unwrap();
        assert!(snapshot.captured_at > 0);
        assert_eq!(snapshot.windows[0].label(), "周额度");
        assert_eq!(snapshot.windows[0].remaining, 39);
        assert_eq!(snapshot.windows[0].resets_at, Some(2000));
        assert_eq!(snapshot.reset_count, Some(4));
        let rows = snapshot.resets.unwrap();
        assert_eq!(rows[0].expires_at, Some(2000));
        assert!(rows[2].never_expires);
        assert!(!rows[3].never_expires);
        value["rateLimitResetCredits"]["credits"] = Value::Null;
        let count_only = parse(&value).unwrap();
        assert_eq!(count_only.reset_count, Some(4));
        assert!(count_only.resets.is_none());
        value["rateLimitResetCredits"]["credits"] = json!([]);
        assert!(parse(&value).unwrap().resets.unwrap().is_empty());
        value["rateLimitResetCredits"] = Value::Null;
        assert_eq!(parse(&value).unwrap().reset_count, None);
        value["rateLimits"]["primary"]["usedPercent"] = json!(140);
        assert_eq!(parse(&value).unwrap().windows[0].remaining, 0);
        value["rateLimitsByLimitId"] = json!({"codex_spark": {"limitName":"Spark", "primary":{
            "usedPercent":3, "windowDurationMins":300}}, "codex":value["rateLimits"].clone()});
        let multi = parse(&value).unwrap();
        assert_eq!(multi.windows[0].bucket, "codex");
        assert_eq!(multi.windows[1].label(), "5 小时额度");
        assert_eq!(multi.windows[1].name, "Spark");
        assert_eq!(countdown(60, 0), "1 分钟");
        assert_eq!(countdown(0, 0), "等待同步");
        assert_eq!(countdown(-1, 0), "等待同步");
        assert!(parse(&json!({})).is_err());
        assert!(parse(&json!({"rateLimits": {}})).is_err());
        let fraction = parse(&json!({"rateLimits":{"primary":{"usedPercent":12.5}}})).unwrap();
        assert_eq!(fraction.windows[0].remaining, 87);
    }
}
