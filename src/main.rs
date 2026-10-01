//! Sits in front of the Minecraft server. While the server is up, connections
//! are passed straight through. While it is down (e.g. nightly restart), pings
//! get a "server is restarting" MOTD and joins get a friendly kick message.

use std::{io, net::SocketAddr, time::Duration};

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{
    io::{copy_bidirectional, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

#[derive(Deserialize)]
#[serde(default)]
struct Config {
    listen: String,
    server: String,
    proxy_protocol: bool,
    motd_line_1: String,
    motd_line_2: String,
    kick_message: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:25565".into(),
            server: "127.0.0.1:25566".into(),
            proxy_protocol: false,
            motd_line_1: "&x&F&A&E&2&0&5&lLEMON&f&lMC&7&l.DE &r<##a9a9a9>• &x&F&A&E&2&0&5&lC&x&F&A&D&7&0&5&lI&x&F&A&C&B&0&5&lT&x&F&A&C&0&0&5&lY&x&F&A&B&4&0&5&lB&x&F&A&A&9&0&5&lU&x&F&A&9&D&0&5&lI&x&F&A&9&2&0&5&lL&x&F&A&8&6&0&5&lD &r<##a9a9a9>(<##e9e487>1.21.11 - 26.3&r<##a9a9a9>)".into(),
            motd_line_2: "                    &c&lDer Server wird neu gestartet".into(),
            kick_message: "&cDer Server wird gerade neu gestartet.\n&7Bitte versuche es in einer Minute erneut.".into(),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> io::Result<()> {
    let path = std::env::args().nth(1).unwrap_or("restart-motd.toml".into());
    let config: Config = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).map_err(io::Error::other)?,
        Err(_) => Config::default(),
    };

    let motd = format!("{}\n{}", config.motd_line_1, config.motd_line_2);
    let motd = to_component(&motd);
    let kick = to_component(&config.kick_message);

    let listener = TcpListener::bind(&config.listen).await?;
    println!("Listening on {}, forwarding to {}", config.listen, config.server);

    let config: &'static Config = Box::leak(Box::new(config));
    loop {
        let (client, addr) = listener.accept().await?;
        let (motd, kick) = (motd.clone(), kick.clone());
        tokio::spawn(async move {
            let _ = handle(client, addr, config, motd, kick).await;
        });
    }
}

async fn handle(mut client: TcpStream, addr: SocketAddr, config: &Config, motd: Value, kick: Value) -> io::Result<()> {
    // Server up? Just pipe everything through.
    if let Ok(Ok(mut server)) = timeout(Duration::from_secs(2), TcpStream::connect(&config.server)).await {
        if config.proxy_protocol {
            server.write_all(&proxy_header(addr, client.local_addr()?)).await?;
        }
        copy_bidirectional(&mut client, &mut server).await?;
        return Ok(());
    }

    // Server down: answer the Minecraft handshake ourselves.
    timeout(Duration::from_secs(10), offline(client, motd, kick)).await?
}

async fn offline(mut c: TcpStream, motd: Value, kick: Value) -> io::Result<()> {
    // Handshake: id, protocol, address, port, next state
    let packet = read_packet(&mut c).await?;
    let mut p = &packet[..];
    let _id = read_varint(&mut p)?;
    let protocol = read_varint(&mut p)?;
    let host_len = read_varint(&mut p)? as usize;
    let next_state = read_varint(&mut p.get(host_len + 2..).ok_or_else(bad)?)?;

    if next_state == 1 {
        loop {
            let packet = read_packet(&mut c).await?;
            match packet.first() {
                // Status request
                Some(0) => {
                    let status = json!({
                        "version": { "name": "Neustart", "protocol": protocol },
                        "players": { "max": 0, "online": 0 },
                        "description": motd,
                    });
                    send(&mut c, 0, status.to_string().as_bytes()).await?;
                }
                // Ping: echo payload back, then done
                Some(1) => return send_raw(&mut c, &packet).await,
                _ => return Ok(()),
            }
        }
    } else {
        // Login: disconnect with message
        send(&mut c, 0, kick.to_string().as_bytes()).await
    }
}

fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "bad packet")
}

// --- Minecraft packet helpers ---

async fn read_packet(r: &mut (impl AsyncRead + Unpin)) -> io::Result<Vec<u8>> {
    let mut len = 0;
    for i in 0..3 {
        let byte = r.read_u8().await?;
        len |= ((byte & 0x7f) as usize) << (7 * i);
        if byte & 0x80 == 0 {
            break;
        }
    }
    if len == 0 || len > 4096 {
        return Err(bad());
    }
    let mut buf = vec![0; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

fn read_varint(buf: &mut &[u8]) -> io::Result<i32> {
    let mut value = 0;
    for i in 0..5 {
        let byte = *buf.first().ok_or_else(bad)?;
        *buf = &buf[1..];
        value |= ((byte & 0x7f) as i32) << (7 * i);
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(bad())
}

fn write_varint(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Send packet `id` whose body is a single string.
async fn send(w: &mut (impl AsyncWrite + Unpin), id: u8, string: &[u8]) -> io::Result<()> {
    let mut body = vec![id];
    write_varint(&mut body, string.len() as u32);
    body.extend_from_slice(string);
    send_raw(w, &body).await
}

/// Send an already built packet body (id + data) with its length prefix.
async fn send_raw(w: &mut (impl AsyncWrite + Unpin), body: &[u8]) -> io::Result<()> {
    let mut out = Vec::new();
    write_varint(&mut out, body.len() as u32);
    out.extend_from_slice(body);
    w.write_all(&out).await
}

// --- PROXY protocol v2 (optional, passes the real player IP to the server) ---

fn proxy_header(src: SocketAddr, dst: SocketAddr) -> Vec<u8> {
    let mut h = b"\r\n\r\n\0\r\nQUIT\n".to_vec();
    match (src, dst) {
        (SocketAddr::V4(s), SocketAddr::V4(d)) => {
            h.extend_from_slice(&[0x21, 0x11, 0, 12]);
            h.extend_from_slice(&s.ip().octets());
            h.extend_from_slice(&d.ip().octets());
            h.extend_from_slice(&s.port().to_be_bytes());
            h.extend_from_slice(&d.port().to_be_bytes());
        }
        (SocketAddr::V6(s), SocketAddr::V6(d)) => {
            h.extend_from_slice(&[0x21, 0x21, 0, 36]);
            h.extend_from_slice(&s.ip().octets());
            h.extend_from_slice(&d.ip().octets());
            h.extend_from_slice(&s.port().to_be_bytes());
            h.extend_from_slice(&d.port().to_be_bytes());
        }
        // Mixed families: send "LOCAL" so the server just ignores the header
        _ => h.extend_from_slice(&[0x20, 0, 0, 0]),
    }
    h
}

// --- Text formatting ---

/// Turns `&c`, `&x&R&R&G&G&B&B` and `<##RRGGBB>` formatted text into a Minecraft JSON text component.
fn to_component(text: &str) -> Value {
    let mut parts: Vec<Value> = Vec::new();
    let (mut color, mut bold, mut italic) = (None::<String>, false, false);
    let mut buf = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    let mut flush = |buf: &mut String, color: &Option<String>, bold: bool, italic: bool| {
        if !buf.is_empty() {
            let mut part = json!({ "text": std::mem::take(buf), "bold": bold, "italic": italic });
            if let Some(color) = color {
                part["color"] = json!(color);
            }
            parts.push(part);
        }
    };

    while i < chars.len() {
        let rest: String = chars[i..].iter().take(10).collect();
        // <##RRGGBB>
        if let Some(hex) = rest.strip_prefix("<##").and_then(|r| r.get(..6)).filter(|h| is_hex(h)) {
            flush(&mut buf, &color, bold, italic);
            color = Some(format!("#{hex}"));
            i += 10;
            continue;
        }
        if chars[i] == '&' && i + 1 < chars.len() {
            let code = chars[i + 1].to_ascii_lowercase();
            // &x&R&R&G&G&B&B
            if code == 'x' {
                let hex: String = (0..6).filter_map(|n| chars.get(i + 3 + n * 2)).collect();
                if hex.len() == 6 && is_hex(&hex) {
                    flush(&mut buf, &color, bold, italic);
                    color = Some(format!("#{hex}"));
                    i += 14;
                    continue;
                }
            }
            if let Some(name) = color_name(code) {
                flush(&mut buf, &color, bold, italic);
                (color, bold, italic) = (Some(name.into()), false, false);
                i += 2;
                continue;
            }
            match code {
                'l' | 'o' | 'r' => {
                    flush(&mut buf, &color, bold, italic);
                    match code {
                        'l' => bold = true,
                        'o' => italic = true,
                        _ => (color, bold, italic) = (None, false, false),
                    }
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        buf.push(chars[i]);
        i += 1;
    }
    flush(&mut buf, &color, bold, italic);

    json!({ "text": "", "extra": parts })
}

fn is_hex(s: &str) -> bool {
    s.chars().all(|c| c.is_ascii_hexdigit())
}

fn color_name(code: char) -> Option<&'static str> {
    Some(match code {
        '0' => "black",
        '1' => "dark_blue",
        '2' => "dark_green",
        '3' => "dark_aqua",
        '4' => "dark_red",
        '5' => "dark_purple",
        '6' => "gold",
        '7' => "gray",
        '8' => "dark_gray",
        '9' => "blue",
        'a' => "green",
        'b' => "aqua",
        'c' => "red",
        'd' => "light_purple",
        'e' => "yellow",
        'f' => "white",
        _ => return None,
    })
}
