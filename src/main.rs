use anyhow::{bail, Context, Result};
use figment::providers::{Env, Format, Serialized, Yaml};
use figment::Figment;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize, Serialize)]
struct Config {
    image: String,
    workdir: String,
    cores: u32,
    memory: String,
    ssh_port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            image: String::new(),
            workdir: "/var/lib/libkrun-builder".into(),
            cores: 6,
            memory: "8GiB".into(),
            ssh_port: 31122,
        }
    }
}

impl Config {
    fn load() -> Result<Self> {
        let config_path = env::var("LIBKRUN_CONFIG")
            .unwrap_or_else(|_| "/etc/libkrun-builder/config.yaml".into());

        // Figment: defaults → YAML config file → env vars (LIBKRUN_ prefix)
        let cfg: Config = Figment::new()
            .merge(Serialized::defaults(Config::default()))
            .merge(Yaml::file(&config_path))
            .merge(Env::prefixed("LIBKRUN_"))
            .extract()
            .context("Failed to load configuration")?;

        Ok(cfg)
    }

    fn memory_mib(&self) -> u32 {
        // Accept plain number (MiB) or strings like "8GiB"/"8192MiB"
        if let Ok(n) = self.memory.parse::<u32>() {
            return n;
        }
        let lower = self.memory.to_lowercase();
        if let Some(s) = lower.strip_suffix("gib") {
            return s.trim().parse::<u32>().unwrap_or(8192) * 1024;
        }
        if let Some(s) = lower.strip_suffix("mib") {
            return s.trim().parse::<u32>().unwrap_or(8192);
        }
        8192
    }

    fn image_path(&self) -> Result<PathBuf> {
        if self.image.is_empty() {
            bail!("'image' is required (set via config file or LIBKRUN_IMAGE env var)");
        }
        let path = PathBuf::from(&self.image);
        if !path.exists() {
            bail!("Guest image not found: {}", self.image);
        }
        Ok(path)
    }

    fn work_dir(&self) -> PathBuf {
        PathBuf::from(&self.workdir)
    }
}

fn pid_path(cfg: &Config, name: &str) -> PathBuf {
    cfg.work_dir().join(format!("{name}.pid"))
}

fn read_pid(cfg: &Config, name: &str) -> Option<u32> {
    fs::read_to_string(pid_path(cfg, name))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn write_pid(cfg: &Config, name: &str, pid: u32) -> Result<()> {
    fs::write(pid_path(cfg, name), pid.to_string())?;
    Ok(())
}

fn is_running(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn ensure_ssh_keys(cfg: &Config) -> Result<()> {
    let dir = cfg.work_dir();
    let key = dir.join("ssh_host_ed25519_key");
    if key.exists() {
        eprintln!("SSH keys already exist");
        return Ok(());
    }
    eprintln!("Generating SSH host keys...");
    let status = Command::new("ssh-keygen")
        .args([
            "-t",
            "ed25519",
            "-f",
            key.to_str().unwrap(),
            "-N",
            "",
            "-C",
            "libkrun-builder",
        ])
        .status()
        .context("Failed to run ssh-keygen")?;
    if !status.success() {
        bail!("ssh-keygen failed");
    }
    Ok(())
}

fn start_gvproxy(cfg: &Config) -> Result<u32> {
    if let Some(pid) = read_pid(cfg, "gvproxy")
        && is_running(pid)
    {
        eprintln!("gvproxy already running (pid {pid})");
        return Ok(pid);
    }

    let dir = cfg.work_dir();
    let sock = dir.join("gvproxy.sock");
    let _ = fs::remove_file(&sock);

    eprintln!(
        "Starting gvproxy (SSH forwarding :{} -> guest:22)...",
        cfg.ssh_port
    );

    let child = Command::new("gvproxy")
        .args([
            "-listen-vfkit",
            &format!("unixgram://{}", sock.display()),
            "-ssh-port",
            &cfg.ssh_port.to_string(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to start gvproxy")?;

    let pid = child.id();
    write_pid(cfg, "gvproxy", pid)?;

    if let Some(stdout) = child.stdout {
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                eprintln!("[gvproxy] {line}");
            }
        });
    }
    if let Some(stderr) = child.stderr {
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("[gvproxy] {line}");
            }
        });
    }

    let start = Instant::now();
    while !sock.exists() {
        if start.elapsed() > Duration::from_secs(10) {
            bail!("gvproxy socket did not appear within 10s");
        }
        thread::sleep(Duration::from_millis(100));
    }

    eprintln!("gvproxy started (pid {pid})");
    Ok(pid)
}

fn start_krunkit(cfg: &Config, image: &Path) -> Result<u32> {
    if let Some(pid) = read_pid(cfg, "krunkit")
        && is_running(pid)
    {
        eprintln!("krunkit already running (pid {pid})");
        return Ok(pid);
    }

    let dir = cfg.work_dir();
    let sock = dir.join("gvproxy.sock");
    let rosetta_dir = "/Library/Apple/usr/libexec/oah";
    let ssh_keys_dir = dir.to_str().unwrap();

    eprintln!(
        "Starting krunkit (cores={}, memory={}MiB, image={})...",
        cfg.cores,
        cfg.memory_mib(),
        image.display()
    );

    let child = Command::new("krunkit")
        .args([
            "--cpus",
            &cfg.cores.to_string(),
            "--mem",
            &cfg.memory_mib().to_string(),
            "--restful-uri",
            "",
            "--virtiofs",
            &format!("rosetta:{rosetta_dir}"),
            "--virtiofs",
            &format!("ssh-keys:{ssh_keys_dir}"),
            "--network",
            &format!("unixgram://{}", sock.display()),
            image.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to start krunkit")?;

    let pid = child.id();
    write_pid(cfg, "krunkit", pid)?;

    if let Some(stdout) = child.stdout {
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                eprintln!("[krunkit] {line}");
            }
        });
    }
    if let Some(stderr) = child.stderr {
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("[krunkit] {line}");
            }
        });
    }

    eprintln!("krunkit started (pid {pid})");
    Ok(pid)
}

fn wait_for_ssh(cfg: &Config) -> Result<()> {
    let addr = format!("127.0.0.1:{}", cfg.ssh_port);
    eprintln!("Waiting for SSH on {addr}...");

    let start = Instant::now();
    let timeout = Duration::from_secs(120);

    loop {
        if start.elapsed() > timeout {
            bail!("SSH did not become available within 120s");
        }
        if TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_secs(2)).is_ok() {
            eprintln!("SSH is reachable on {addr}");
            return Ok(());
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn cmd_start(cfg: &Config) -> Result<()> {
    let dir = cfg.work_dir();
    fs::create_dir_all(&dir).context("Failed to create working directory")?;

    let image = cfg.image_path()?;
    ensure_ssh_keys(cfg)?;
    start_gvproxy(cfg)?;
    start_krunkit(cfg, &image)?;
    wait_for_ssh(cfg)?;

    eprintln!("libkrun-builder is ready");

    // Keep the daemon alive — launchd expects the process to stay running.
    loop {
        if let Some(pid) = read_pid(cfg, "krunkit")
            && !is_running(pid)
        {
            eprintln!("krunkit (pid {pid}) exited, shutting down");
            cmd_stop(cfg)?;
            bail!("krunkit exited unexpectedly");
        }
        thread::sleep(Duration::from_secs(5));
    }
}

fn kill_pid(cfg: &Config, name: &str) {
    if let Some(pid) = read_pid(cfg, name) {
        if is_running(pid) {
            eprintln!("Stopping {name} (pid {pid})...");
            let _ = Command::new("kill").arg(pid.to_string()).status();
            let start = Instant::now();
            while is_running(pid) && start.elapsed() < Duration::from_secs(5) {
                thread::sleep(Duration::from_millis(200));
            }
            if is_running(pid) {
                eprintln!("{name} did not exit gracefully, sending SIGKILL");
                let _ = Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .status();
            }
        }
        let _ = fs::remove_file(pid_path(cfg, name));
    }
}

fn cmd_stop(cfg: &Config) -> Result<()> {
    eprintln!("Attempting graceful shutdown via SSH...");
    let _ = Command::new("ssh")
        .args([
            "-o",
            "ConnectTimeout=3",
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-p",
            &cfg.ssh_port.to_string(),
            "-i",
            &cfg.work_dir()
                .join("ssh_host_ed25519_key")
                .to_string_lossy(),
            "root@127.0.0.1",
            "poweroff",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    thread::sleep(Duration::from_secs(3));

    kill_pid(cfg, "krunkit");
    kill_pid(cfg, "gvproxy");

    let _ = fs::remove_file(cfg.work_dir().join("gvproxy.sock"));

    eprintln!("libkrun-builder stopped");
    Ok(())
}

fn cmd_status(cfg: &Config) -> Result<()> {
    let mut running = true;

    if let Some(pid) = read_pid(cfg, "gvproxy") {
        if is_running(pid) {
            println!("gvproxy: running (pid {pid})");
        } else {
            println!("gvproxy: dead (stale pid {pid})");
            running = false;
        }
    } else {
        println!("gvproxy: not running");
        running = false;
    }

    if let Some(pid) = read_pid(cfg, "krunkit") {
        if is_running(pid) {
            println!("krunkit: running (pid {pid})");
        } else {
            println!("krunkit: dead (stale pid {pid})");
            running = false;
        }
    } else {
        println!("krunkit: not running");
        running = false;
    }

    let addr = format!("127.0.0.1:{}", cfg.ssh_port);
    let ssh_ok = TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_secs(2)).is_ok();
    if ssh_ok {
        println!("ssh: reachable on {addr}");
    } else {
        println!("ssh: not reachable on {addr}");
        running = false;
    }

    if running {
        println!("\nlibkrun-builder: healthy");
    } else {
        println!("\nlibkrun-builder: unhealthy");
        std::process::exit(1);
    }
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("help");

    match cmd {
        "start" | "stop" | "status" => {
            let cfg = Config::load()?;
            eprintln!("Config: {:?}", cfg);
            match cmd {
                "start" => cmd_start(&cfg),
                "stop" => cmd_stop(&cfg),
                "status" => cmd_status(&cfg),
                _ => unreachable!(),
            }
        }
        _ => {
            eprintln!("Usage: libkrun-builder <start|stop|status>");
            eprintln!();
            eprintln!("Configuration (Figment: defaults -> YAML -> env vars):");
            eprintln!("  Config file: LIBKRUN_CONFIG (default: /etc/libkrun-builder/config.yaml)");
            eprintln!("  LIBKRUN_IMAGE    Path to NixOS guest qcow2 image");
            eprintln!("  LIBKRUN_WORKDIR  Working directory (default: /var/lib/libkrun-builder)");
            eprintln!("  LIBKRUN_CORES    CPU cores for VM (default: 6)");
            eprintln!("  LIBKRUN_MEMORY   Memory for VM, e.g. '8GiB' (default: 8GiB)");
            eprintln!("  LIBKRUN_SSH_PORT SSH port forwarding (default: 31122)");
            std::process::exit(if cmd == "help" { 0 } else { 1 });
        }
    }
}
