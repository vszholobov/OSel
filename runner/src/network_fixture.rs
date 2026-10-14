//! Own the local server process for a network test; always reap it, including failures.
use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
};
pub struct Server {
    child: Child,
    pub guest_address: String,
}
impl Server {
    pub fn start(root: &Path, bot: bool) -> Result<Self, String> {
        let binary = root.join("target/network-fixture");
        let status = Command::new("go")
            .current_dir(root.join("tools/network-fixture"))
            .env_remove("GOOS")
            .env_remove("GOARCH")
            .env_remove("GOAMD64")
            .env("GOTOOLCHAIN", "local")
            .args(["build", "-mod=readonly", "-o"])
            .arg(&binary)
            .arg(".")
            .status()
            .map_err(|e| format!("build local Tetris server: {e}"))?;
        if !status.success() {
            return Err("could not build local Tetris server".into());
        }
        let mut command = Command::new(binary);
        if bot {
            command.arg("-bot");
        }
        let mut child = command
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let stdout = child.stdout.take().unwrap();
        let mut server = Self {
            child,
            guest_address: String::new(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let result = BufReader::new(stdout)
                .read_line(&mut line)
                .map(|_| line)
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        let line = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|_| "local Tetris server did not start within 10 seconds")??;
        server.guest_address = guest_address(line.trim())?;
        Ok(server)
    }
}
fn guest_address(line: &str) -> Result<String, String> {
    let address = line
        .strip_prefix("NET SERVER 127.0.0.1:")
        .ok_or_else(|| format!("invalid server banner: {line}"))?;
    let port: u16 = address.parse().map_err(|_| "invalid local server port")?;
    if port == 0 {
        return Err("local server returned port zero".into());
    }
    Ok(format!("10.0.2.2:{port}"))
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_only_valid_loopback_banner() {
        assert_eq!(
            guest_address("NET SERVER 127.0.0.1:45678").unwrap(),
            "10.0.2.2:45678"
        );
        for s in [
            "NET SERVER 127.0.0.1:0",
            "NET SERVER 127.0.0.1:65536",
            "NET SERVER 0.0.0.0:8080",
            "error",
        ] {
            assert!(guest_address(s).is_err());
        }
    }
}
