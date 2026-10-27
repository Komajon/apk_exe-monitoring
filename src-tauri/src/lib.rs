use serde::Serialize;
use std::time::Instant;

#[derive(Serialize)]
struct PingResult {
    ok: bool,
    ms: u64,
}

/// Допускаем только IPv4 / имя хоста.
fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && !h.starts_with('-')
        && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

#[tauri::command]
async fn ping_host(ip: String, timeout_ms: u64) -> Result<PingResult, String> {
    if !valid_host(&ip) {
        return Err("bad host".into());
    }
    let timeout_ms = timeout_ms.clamp(200, 30_000);
    let t0 = Instant::now();
    let ok = do_ping(&ip, timeout_ms).await;
    Ok(PingResult { ok, ms: t0.elapsed().as_millis() as u64 })
}

/* ---------- Windows / desktop: системный ping ---------- */
#[cfg(not(target_os = "android"))]
async fn do_ping(ip: &str, timeout_ms: u64) -> bool {
    use std::time::Duration;
    use tokio::process::Command;

    let mut cmd = Command::new("ping");
    #[cfg(windows)]
    {
        cmd.args(["-n", "1", "-w", &timeout_ms.to_string(), ip]);
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    {
        let secs = ((timeout_ms + 999) / 1000).to_string();
        cmd.args(["-c", "1", "-W", &secs, ip]);
    }
    cmd.kill_on_drop(true);

    match tokio::time::timeout(Duration::from_millis(timeout_ms + 2_000), cmd.output()).await {
        Ok(Ok(o)) => {
            let text = String::from_utf8_lossy(&o.stdout).to_lowercase();
            o.status.success() && text.contains("ttl=")
        }
        _ => false,
    }
}

/* ---------- Android: ICMP через datagram-сокет (без root) ---------- */
#[cfg(target_os = "android")]
async fn do_ping(ip: &str, timeout_ms: u64) -> bool {
    let h = ip.to_string();
    let r = tokio::task::spawn_blocking(move || android_icmp::echo(&h, timeout_ms))
        .await
        .ok()
        .flatten();
    match r {
        Some(v) => v,
        None => android_icmp::tcp_alive(ip, timeout_ms).await, // ICMP-сокет недоступен
    }
}

#[cfg(target_os = "android")]
mod android_icmp {
    use std::net::{SocketAddr, ToSocketAddrs};
    use std::time::{Duration, Instant};

    fn checksum(d: &[u8]) -> u16 {
        let mut s: u32 = 0;
        for c in d.chunks(2) {
            s += ((c[0] as u32) << 8) | *c.get(1).unwrap_or(&0) as u32;
        }
        while s >> 16 != 0 {
            s = (s & 0xffff) + (s >> 16);
        }
        !(s as u16)
    }

    /// Some(true/false) — результат пинга; None — ICMP-сокеты недоступны.
    pub fn echo(host: &str, timeout_ms: u64) -> Option<bool> {
        let addr = (host, 0u16).to_socket_addrs().ok()?.find_map(|a| match a {
            SocketAddr::V4(v) => Some(*v.ip()),
            _ => None,
        });
        let addr = match addr {
            Some(a) => a,
            None => return Some(false), // имя не резолвится
        };
        unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, libc::IPPROTO_ICMP);
            if fd < 0 {
                return None;
            }
            let mut pkt = [0u8; 16];
            pkt[0] = 8; // echo request
            pkt[4] = 0x42;
            pkt[5] = 0x41;
            pkt[7] = 1;
            for i in 8..16 {
                pkt[i] = i as u8;
            }
            let cs = checksum(&pkt);
            pkt[2] = (cs >> 8) as u8;
            pkt[3] = (cs & 0xff) as u8;

            let mut sa: libc::sockaddr_in = std::mem::zeroed();
            sa.sin_family = libc::AF_INET as libc::sa_family_t;
            sa.sin_addr = libc::in_addr { s_addr: u32::from_ne_bytes(addr.octets()) };
            let sent = libc::sendto(
                fd,
                pkt.as_ptr() as *const libc::c_void,
                pkt.len(),
                0,
                &sa as *const libc::sockaddr_in as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            );
            if sent < 0 {
                libc::close(fd);
                return None;
            }
            let deadline = Instant::now() + Duration::from_millis(timeout_ms);
            let res = loop {
                let now = Instant::now();
                if now >= deadline {
                    break false;
                }
                let rem = deadline - now;
                let tv = libc::timeval {
                    tv_sec: rem.as_secs() as _,
                    tv_usec: rem.subsec_micros() as _,
                };
                libc::setsockopt(
                    fd,
                    libc::SOL_SOCKET,
                    libc::SO_RCVTIMEO,
                    &tv as *const libc::timeval as *const libc::c_void,
                    std::mem::size_of::<libc::timeval>() as libc::socklen_t,
                );
                let mut buf = [0u8; 128];
                let n = libc::recv(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len(), 0);
                if n < 0 {
                    break false;
                }
                if n >= 1 && buf[0] == 0 {
                    break true; // echo reply
                }
            };
            libc::close(fd);
            Some(res)
        }
    }

    /// Запасной вариант: TCP. «Соединение отклонено» тоже значит, что хост жив.
    pub async fn tcp_alive(host: &str, timeout_ms: u64) -> bool {
        let (tx, mut rx) = tokio::sync::mpsc::channel::<bool>(4);
        for p in [80u16, 443, 22, 8080] {
            let tx = tx.clone();
            let h = host.to_string();
            tokio::spawn(async move {
                let r = tokio::time::timeout(
                    Duration::from_millis(timeout_ms),
                    tokio::net::TcpStream::connect((h.as_str(), p)),
                )
                .await;
                let alive = match r {
                    Ok(Ok(_)) => true,
                    Ok(Err(e)) => e.kind() == std::io::ErrorKind::ConnectionRefused,
                    Err(_) => false,
                };
                let _ = tx.send(alive).await;
            });
        }
        drop(tx);
        while let Some(a) = rx.recv().await {
            if a {
                return true;
            }
        }
        false
    }
}

/// hosts.json рядом с exe (на Android файла нет — фронтенд берёт встроенный).
#[tauri::command]
fn load_hosts() -> Option<String> {
    let mut paths = vec![];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            paths.push(dir.join("hosts.json"));
        }
    }
    paths.push(std::path::PathBuf::from("hosts.json"));
    for p in paths {
        if let Ok(s) = std::fs::read_to_string(&p) {
            return Some(s.trim_start_matches('\u{feff}').to_string());
        }
    }
    None
}

/// Открыть веб-интерфейс точки во внешнем браузере.
#[tauri::command]
fn open_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    let host = url.strip_prefix("http://").ok_or("only http")?.trim_end_matches('/');
    if !valid_host(host) {
        return Err("bad host".into());
    }
    let full = format!("http://{host}");
    let _ = &app;

    #[cfg(target_os = "android")]
    {
        use tauri_plugin_opener::OpenerExt;
        app.opener().open_url(full, None::<&str>).map_err(|e| e.to_string())?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .args(["/C", "start", "", &full])
            .creation_flags(0x0800_0000)
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    #[cfg(all(not(windows), not(target_os = "android")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(full).spawn();
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default();
    #[cfg(target_os = "android")]
    let builder = builder.plugin(tauri_plugin_opener::init());
    builder
        .invoke_handler(tauri::generate_handler![ping_host, load_hosts, open_url])
        .run(tauri::generate_context!())
        .expect("error while running application");
}
