use anyhow::{bail, Context, Result};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn work_dir() -> PathBuf {
    PathBuf::from(env::var("LIBKRUN_WORKDIR").unwrap_or_else(|_| "/var/lib/libkrun-builder".into()))
}

fn ssh_port() -> u16 {
    env::var("LIBKRUN_SSH_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(31122)
}

fn cores() -> u32 {
    env::var("LIBKRUN_CORES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6)
}

fn memory() -> u32 {
    let raw = env::var("LIBKRUN_MEMORY").unwrap_or_else(|_| "8192".into());
    // Accept plain number (MiB) or strings like "8GiB"/"8192MiB"
    if let Ok(n) = raw.parse::<u32>() {
        return n;
    }
    let lower = raw.to_lowercase();
    if let Some(s) = lower.strip_suffix("gib") {
        return s.trim().parse::<u32>().unwrap_or(8192) * 1024;
    }
    if let Some(s) = lower.strip_suffix("mib") {
        return s.trim().parse::<u32>().unwrap_or(8192);
    }
    8192
}

fn image_path() -> Result<PathBuf> {
    let p = env::var("LIBKRUN_IMAGE").context("LIBKRUN_IMAGE env var is required")?;
    let path = PathBuf::from(&p);
    if !path.exists() {
        bail!("Guest image not found: {p}");
    }
    Ok(path)
}

fn pid_path(name: &str) -> PathBuf {
    work_dir().join(format!("{name}.pid"))
}

fn read_pid(name: &str) -> Option<u32> {
    fs::read_to_string(pid_path(name))
        .ok()
        .and_then(|s| s.trim().parse().ok())
}

fn write_pid(name: &str, pid: u32) -> Result<()> {
    fs::write(pid_path(name), pid.to_string())?;
    Ok(())
}

fn is_running(pid: u32) -> bool {
    // kill -0 checks process existence without sending a signal
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn ensure_ssh_keys() -> Result<()> {
    let dir = work_dir();
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

fn start_gvproxy() -> Result<u32> {
    if let Some(pid) = read_pid("gvproxy") {
        if is_running(pid) {
            eprintln!("gvproxy already running (pid {pid})");
            return Ok(pid);
        }
    }

    let dir = work_dir();
    let sock = dir.join("gvproxy.sock");
    // Clean up stale socket
    let _ = fs::remove_file(&sock);

    let port = ssh_port();
    eprintln!("Starting gvproxy (SSH forwarding :{port} -> guest:22)...");

    let child = Command::new("gvproxy")
        .args([
            "-listen-vfkit",
            &format!("unixgram://{}", sock.display()),
            "-ssh-port",
            &port.to_string(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to start gvproxy")?;

    let pid = child.id();
    write_pid("gvproxy", pid)?;

    // Spawn log forwarders so gvproxy output goes to our stderr
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

    // Wait for socket to appear
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

fn start_krunkit(image: &Path) -> Result<u32> {
    if let Some(pid) = read_pid("krunkit") {
        if is_running(pid) {
            eprintln!("krunkit already running (pid {pid})");
            return Ok(pid);
        }
    }

    let dir = work_dir();
    let sock = dir.join("gvproxy.sock");
    let rosetta_dir = "/Library/Apple/usr/libexec/oah";
    let ssh_keys_dir = dir.to_str().unwrap();

    eprintln!(
        "Starting krunkit (cores={}, memory={}MiB, image={})...",
        cores(),
        memory(),
        image.display()
    );

    let child = Command::new("krunkit")
        .args([
            "--cpus",
            &cores().to_string(),
            "--mem",
            &memory().to_string(),
            "--restful-uri",
            "",
            // virtiofs: Rosetta runtime
            "--virtiofs",
            &format!("rosetta:{rosetta_dir}"),
            // virtiofs: SSH keys directory
            "--virtiofs",
            &format!("ssh-keys:{ssh_keys_dir}"),
            // virtio-net via gvproxy
            "--network",
            &format!("unixgram://{}", sock.display()),
            // Guest disk image
            image.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to start krunkit")?;

    let pid = child.id();
    write_pid("krunkit", pid)?;

    // Forward krunkit logs
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

fn wait_for_ssh() -> Result<()> {
    let port = ssh_port();
    let addr = format!("127.0.0.1:{port}");
    eprintln!("Waiting for SSH on {addr}...");

    let start = Instant::now();
    let timeout = Duration::from_secs(120);

    loop {
        if start.elapsed() > timeout {
            bail!("SSH did not become available within 120s");
        }
        // Quick TCP connect check — faster than spawning ssh
        if TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_secs(2)).is_ok() {
            eprintln!("SSH is reachable on {addr}");
            return Ok(());
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn cmd_start() -> Result<()> {
    let dir = work_dir();
    fs::create_dir_all(&dir).context("Failed to create working directory")?;

    let image = image_path()?;
    ensure_ssh_keys()?;
    start_gvproxy()?;
    start_krunkit(&image)?;
    wait_for_ssh()?;

    eprintln!("libkrun-builder is ready");

    // Keep the daemon alive — launchd expects the process to stay running.
    // Wait for krunkit to exit (VM shutdown/crash), then exit non-zero
    // so launchd KeepAlive restarts us.
    loop {
        if let Some(pid) = read_pid("krunkit") {
            if !is_running(pid) {
                eprintln!("krunkit (pid {pid}) exited, shutting down");
                cmd_stop()?;
                bail!("krunkit exited unexpectedly");
            }
        }
        thread::sleep(Duration::from_secs(5));
    }
}

fn kill_pid(name: &str) {
    if let Some(pid) = read_pid(name) {
        if is_running(pid) {
            eprintln!("Stopping {name} (pid {pid})...");
            let _ = Command::new("kill")
                .arg(pid.to_string())
                .status();
            // Wait up to 5s for graceful exit
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
        let _ = fs::remove_file(pid_path(name));
    }
}

fn cmd_stop() -> Result<()> {
    // Try graceful SSH poweroff first
    let port = ssh_port();
    eprintln!("Attempting graceful shutdown via SSH...");
    let _ = Command::new("ssh")
        .args([
            "-o", "ConnectTimeout=3",
            "-o", "StrictHostKeyChecking=no",
            "-o", "UserKnownHostsFile=/dev/null",
            "-p", &port.to_string(),
            "-i", &work_dir().join("ssh_host_ed25519_key").to_string_lossy(),
            "root@127.0.0.1",
            "poweroff",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    // Give the VM a few seconds to shut down
    thread::sleep(Duration::from_secs(3));

    kill_pid("krunkit");
    kill_pid("gvproxy");

    // Clean up socket
    let _ = fs::remove_file(work_dir().join("gvproxy.sock"));

    eprintln!("libkrun-builder stopped");
    Ok(())
}

fn cmd_status() -> Result<()> {
    let mut running = true;

    if let Some(pid) = read_pid("gvproxy") {
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

    if let Some(pid) = read_pid("krunkit") {
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

    let port = ssh_port();
    let addr = format!("127.0.0.1:{port}");
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
        "start" => cmd_start(),
        "stop" => cmd_stop(),
        "status" => cmd_status(),
        _ => {
            eprintln!("Usage: libkrun-builder <start|stop|status>");
            eprintln!();
            eprintln!("Environment variables:");
            eprintln!("  LIBKRUN_IMAGE    Path to NixOS guest qcow2 image (required for start)");
            eprintln!("  LIBKRUN_WORKDIR  Working directory (default: /var/lib/libkrun-builder)");
            eprintln!("  LIBKRUN_CORES    CPU cores for VM (default: 6)");
            eprintln!("  LIBKRUN_MEMORY   Memory for VM in MiB or 'NGiB' (default: 8192)");
            eprintln!("  LIBKRUN_SSH_PORT SSH port forwarding (default: 31122)");
            std::process::exit(if cmd == "help" { 0 } else { 1 });
        }
    }
}
