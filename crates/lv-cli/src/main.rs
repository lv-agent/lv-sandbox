//! lv-sandbox CLI (`lvs`) — manage jobs, sessions, files, snapshots, volumes.
//!
//! Global flags: `--url` / `LVSANDBOX_URL`, `--api-key` / `LVSANDBOX_API_KEY`.

use std::io::Write;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use serde::{Deserialize, Serialize};

#[derive(Parser)]
#[command(name = "lvs", version, about = "lv-sandbox CLI")]
struct Cli {
    #[arg(long, env = "LVSANDBOX_URL", default_value = "http://127.0.0.1:8080", global = true)]
    url: String,
    #[arg(long, env = "LVSANDBOX_API_KEY", global = true)]
    api_key: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Worker status
    Status,
    /// List profiles
    Profiles,
    /// One-shot jobs
    Jobs {
        #[command(subcommand)]
        cmd: JobsCmd,
    },
    /// Persistent sessions
    Sessions {
        #[command(subcommand)]
        cmd: SessionsCmd,
    },
    /// Run a command in a session
    Exec(ExecArgs),
    /// Interactive shell (PTY over WebSocket)
    Shell { id: String, argv: Vec<String> },
    /// Session files
    Files {
        #[command(subcommand)]
        cmd: FilesCmd,
    },
    /// Snapshots
    Snapshots {
        #[command(subcommand)]
        cmd: SnapshotsCmd,
    },
    /// Volumes
    Volumes {
        #[command(subcommand)]
        cmd: VolumesCmd,
    },
}

#[derive(Subcommand)]
enum JobsCmd {
    /// Run argv (blocks until done)
    Run {
        argv: Vec<String>,
        #[arg(long, default_value = "shell")]
        profile: String,
        #[arg(long)]
        timeout: Option<String>,
    },
}

#[derive(Subcommand)]
enum SessionsCmd {
    /// List sessions
    Ls,
    /// Create a session
    New {
        #[arg(long, default_value = "shell")]
        profile: String,
        #[arg(long)]
        from_snapshot: Option<String>,
        /// repeatable, format name:mount
        #[arg(long)]
        volume: Vec<String>,
    },
    /// Destroy a session
    Rm { id: String },
}

#[derive(Args)]
struct ExecArgs {
    /// session id
    id: String,
    /// argv after `--`
    argv: Vec<String>,
    #[arg(long)]
    timeout: Option<String>,
}

#[derive(Subcommand)]
enum FilesCmd {
    /// Upload a local file
    Put { id: String, path: String, local: String },
    /// Download (to stdout, or --out <file>)
    Get {
        id: String,
        path: String,
        #[arg(long)]
        out: Option<String>,
    },
    /// List a directory (default: workspace root)
    Ls {
        id: String,
        #[arg(default_value = "")]
        path: String,
    },
}

#[derive(Subcommand)]
enum SnapshotsCmd {
    Ls,
    /// Create a snapshot of a session
    New { id: String },
    Rm { id: String },
}

#[derive(Subcommand)]
enum VolumesCmd {
    Ls,
    New { name: String },
    Rm { name: String },
}

// ----- response shapes -----
#[derive(Deserialize)]
struct JobResp {
    status: String,
    exit_code: Option<i32>,
    stdout: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    stderr: Option<String>,
}

#[derive(Deserialize)]
struct CreateResp {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    snapshot_id: Option<String>,
}

#[derive(Deserialize)]
struct SessionRow {
    session_id: String,
    #[serde(default)]
    profile: String,
}
#[derive(Deserialize)]
struct SessionsResp {
    #[serde(default)]
    sessions: Vec<SessionRow>,
}

#[derive(Deserialize)]
struct FileRow {
    name: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    is_dir: bool,
}
#[derive(Deserialize)]
struct FilesResp {
    #[serde(default)]
    entries: Vec<FileRow>,
}

#[derive(Deserialize)]
struct StringsResp {
    #[serde(default)]
    profiles: Vec<String>,
    #[serde(default)]
    snapshots: Vec<String>,
    #[serde(default)]
    volumes: Vec<String>,
}

struct LvClient {
    http: reqwest::Client,
    base: String,
}

impl LvClient {
    fn new(base: &str, api_key: Option<&str>) -> Self {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Some(k) = api_key {
            if let Ok(v) = reqwest::header::HeaderValue::from_str(&format!("Bearer {k}")) {
                headers.insert(reqwest::header::AUTHORIZATION, v);
            }
        }
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(Duration::from_secs(300))
            .build()
            .expect("http client build");
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        Ok(self
            .http
            .get(self.url(path))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn post_json<B: Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        Ok(self
            .http
            .post(self.url(path))
            .json(body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn delete(&self, path: &str) -> Result<()> {
        self.http
            .delete(self.url(path))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn put_raw(&self, path: &str, data: Vec<u8>) -> Result<()> {
        self.http
            .put(self.url(path))
            .body(data)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn get_raw(&self, path: &str) -> Result<Vec<u8>> {
        Ok(self
            .http
            .get(self.url(path))
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await?
            .to_vec())
    }

    /// Submit a one-shot job and poll until terminal.
    async fn job_run(
        &self,
        argv: &[String],
        profile: &str,
        timeout: &Option<String>,
    ) -> Result<JobResp> {
        let mut body = serde_json::json!({
            "argv": argv,
            "profile_name": profile,
            "job_id": format!("cli-{}", std::process::id()),
        });
        if let Some(t) = timeout {
            body["timeout"] = serde_json::json!(t);
        }
        let r: CreateResp = self.post_json("/api/v1/jobs", &body).await?;
        let jid = r
            .session_id
            .or_else(|| body["job_id"].as_str().map(|s| s.to_string()))
            .ok_or_else(|| anyhow!("server returned no job_id"))?;
        loop {
            let jr: JobResp = self.get_json(&format!("/api/v1/jobs/{jid}")).await?;
            if jr.status != "Running" {
                return Ok(jr);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

fn print_out(r: &JobResp) {
    if let Some(s) = &r.stdout {
        print!("{s}");
        let _ = std::io::stdout().flush();
    }
}

/// cr-033: 交互 shell——WebSocket + raw terminal。
async fn run_shell(base: &str, _api_key: Option<&str>, sid: &str, argv: &[String]) -> Result<()> {
    use futures_util::{SinkExt, StreamExt};
    use std::os::unix::io::AsRawFd;

    let ws_base = base
        .replacen("http://", "ws://", 1)
        .replacen("https://", "wss://", 1);
    let mut url = format!("{}/api/v1/sessions/{}/tty?argv=", ws_base.trim_end_matches('/'), sid);
    let argv_str = argv.join("+");
    url.push_str(&argv_str);

    let (ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .context("WebSocket connect")?;
    let (mut ws_tx, mut ws_rx) = ws.split();

    // raw terminal
    let stdin_fd = std::io::stdin().as_raw_fd();
    let mut orig: libc::termios = unsafe { std::mem::zeroed() };
    let raw_ok = unsafe { libc::tcgetattr(stdin_fd, &mut orig) } == 0;
    if raw_ok {
        let mut raw = orig;
        unsafe { libc::cfmakeraw(&mut raw); }
        unsafe { libc::tcsetattr(stdin_fd, libc::TCSANOW, &raw); }
    }

    let result: Result<()> = async {
        use tokio::io::AsyncReadExt;
        let mut stdin = tokio::io::stdin();
        let mut stdout = std::io::stdout();
        let mut buf = [0u8; 1024];
        loop {
            tokio::select! {
                n = stdin.read(&mut buf) => {
                    let n = n?;
                    if n == 0 { break; }
                    if ws_tx.send(tokio_tungstenite::tungstenite::Message::binary(buf[..n].to_vec())).await.is_err() { break; }
                }
                msg = ws_rx.next() => {
                    match msg {
                        Some(Ok(m)) => match m {
                            tokio_tungstenite::tungstenite::Message::Binary(data) => {
                                use std::io::Write;
                                let _ = stdout.write_all(&data);
                                let _ = stdout.flush();
                            }
                            tokio_tungstenite::tungstenite::Message::Text(s) => {
                                // control message (exit/timeout/error)?
                                if s.contains("\"type\":\"exit\"")
                                    || s.contains("\"type\":\"timeout\"")
                                    || s.contains("\"type\":\"error\"")
                                {
                                    eprintln!("{s}");
                                    break;
                                }
                                use std::io::Write;
                                let _ = stdout.write_all(s.as_bytes());
                                let _ = stdout.flush();
                            }
                            tokio_tungstenite::tungstenite::Message::Close(_) => break,
                            _ => {}
                        },
                        _ => break,
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    if raw_ok {
        unsafe { libc::tcsetattr(stdin_fd, libc::TCSANOW, &orig); }
    }
    result
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = LvClient::new(&cli.url, cli.api_key.as_deref());

    match cli.cmd {
        Cmd::Status => {
            let v: serde_json::Value = client.get_json("/api/v1/status").await?;
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Cmd::Profiles => {
            let r: StringsResp = client.get_json("/api/v1/profiles").await?;
            for p in r.profiles {
                println!("{p}");
            }
        }
        Cmd::Jobs {
            cmd: JobsCmd::Run { argv, profile, timeout },
        } => {
            if argv.is_empty() {
                return Err(anyhow!("no argv given"));
            }
            let r = client.job_run(&argv, &profile, &timeout).await?;
            print_out(&r);
            std::process::exit(r.exit_code.unwrap_or(0));
        }
        Cmd::Sessions { cmd } => match cmd {
            SessionsCmd::Ls => {
                let s: SessionsResp = client.get_json("/api/v1/sessions").await?;
                for r in s.sessions {
                    println!("{}\t{}", r.session_id, r.profile);
                }
            }
            SessionsCmd::New { profile, from_snapshot, volume } => {
                let mut body = serde_json::json!({ "profile_name": profile });
                if let Some(s) = from_snapshot {
                    body["from_snapshot"] = serde_json::json!(s);
                }
                if !volume.is_empty() {
                    let vols: Vec<_> = volume
                        .iter()
                        .map(|v| {
                            let mut it = v.splitn(2, ':');
                            let name = it.next().unwrap_or("").to_string();
                            let mount = it.next().unwrap_or(&name).to_string();
                            serde_json::json!({"name": name, "mount": mount})
                        })
                        .collect();
                    body["volumes"] = serde_json::json!(vols);
                }
                let r: CreateResp = client.post_json("/api/v1/sessions", &body).await?;
                println!("{}", r.session_id.ok_or_else(|| anyhow!("no session_id"))?);
            }
            SessionsCmd::Rm { id } => client.delete(&format!("/api/v1/sessions/{id}")).await?,
        },
        Cmd::Exec(a) => {
            if a.argv.is_empty() {
                return Err(anyhow!("no argv given"));
            }
            let mut body = serde_json::json!({ "argv": a.argv });
            if let Some(t) = a.timeout {
                body["timeout"] = serde_json::json!(t);
            }
            let r: JobResp = client
                .post_json(&format!("/api/v1/sessions/{}/exec", a.id), &body)
                .await?;
            print_out(&r);
            std::process::exit(r.exit_code.unwrap_or(0));
        }
        Cmd::Shell { id, argv } => {
            if argv.is_empty() {
                return Err(anyhow!("no argv given"));
            }
            run_shell(&cli.url, cli.api_key.as_deref(), &id, &argv).await?;
        }
        Cmd::Files { cmd } => match cmd {
            FilesCmd::Put { id, path, local } => {
                let data = std::fs::read(&local).with_context(|| format!("read {local}"))?;
                client.put_raw(&format!("/api/v1/sessions/{id}/files/{path}"), data).await?;
            }
            FilesCmd::Get { id, path, out } => {
                let data = client.get_raw(&format!("/api/v1/sessions/{id}/files/{path}")).await?;
                match out {
                    Some(f) => std::fs::write(&f, &data)?,
                    None => std::io::stdout().write_all(&data)?,
                }
            }
            FilesCmd::Ls { id, path } => {
                let mut req = client
                    .http
                    .get(client.url(&format!("/api/v1/sessions/{id}/files")));
                if !path.is_empty() {
                    req = req.query(&[("path", path.as_str())]);
                }
                let fr: FilesResp = req.send().await?.error_for_status()?.json().await?;
                for e in fr.entries {
                    println!("{}\t{}\t{}", if e.is_dir { "d" } else { "f" }, e.size, e.name);
                }
            }
        },
        Cmd::Snapshots { cmd } => match cmd {
            SnapshotsCmd::Ls => {
                let r: StringsResp = client.get_json("/api/v1/snapshots").await?;
                for s in r.snapshots {
                    println!("{s}");
                }
            }
            SnapshotsCmd::New { id } => {
                let r: CreateResp = client
                    .post_json(
                        &format!("/api/v1/sessions/{id}/snapshot"),
                        &serde_json::json!({}),
                    )
                    .await?;
                println!("{}", r.snapshot_id.ok_or_else(|| anyhow!("no snapshot_id"))?);
            }
            SnapshotsCmd::Rm { id } => client.delete(&format!("/api/v1/snapshots/{id}")).await?,
        },
        Cmd::Volumes { cmd } => match cmd {
            VolumesCmd::Ls => {
                let r: StringsResp = client.get_json("/api/v1/volumes").await?;
                for v in r.volumes {
                    println!("{v}");
                }
            }
            VolumesCmd::New { name } => {
                let _: serde_json::Value =
                    client.post_json("/api/v1/volumes", &serde_json::json!({"name": name})).await?;
            }
            VolumesCmd::Rm { name } => client.delete(&format!("/api/v1/volumes/{name}")).await?,
        },
    }
    Ok(())
}
