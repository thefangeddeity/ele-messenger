//! The page's Terminal: a shell on another machine, through the mesh. This
//! machine runs ssh in a pseudo-terminal, through `elemesh ssh-proxy`, and
//! streams it to the browser over a websocket. Anyone signed in to the page
//! may open one: the mesh passphrase is the key to the household's machines.
//! The far machine still answers only if SSH over the mesh is on there.

use anyhow::{ensure, Context, Result};
use axum::extract::ws::{Message, WebSocket};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use tokio::sync::mpsc;

pub async fn run(mut ws: WebSocket, machine: String) {
    if let Err(e) = session(&mut ws, &machine).await {
        let _ = ws.send(Message::Binary(format!("\r\n[{e:#}]\r\n").into_bytes().into())).await;
    }
    let _ = ws.send(Message::Close(None)).await;
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }
}

async fn session(ws: &mut WebSocket, machine: &str) -> Result<()> {
    // It becomes an ssh argument: a machine id or name, nothing else.
    ensure!(
        !machine.is_empty() && machine.len() <= 64 && machine.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
        "that isn't a machine name"
    );
    let exe = std::env::current_exe()?;
    let pty = native_pty_system().openpty(size(80, 24))?;
    let mut cmd = CommandBuilder::new("ssh");
    // -C: terminal output is repetitive escape codes, and a Bluetooth link
    // carries a few KB/s, so compressing costs far less than it saves.
    cmd.args([
        "-tt",
        "-C",
        "-o",
        &format!("ProxyCommand=\"{}\" ssh-proxy %h", exe.display()),
        "-o",
        &format!("HostKeyAlias=ele-{machine}"),
        "-o",
        "StrictHostKeyChecking=accept-new",
        machine,
    ]);
    cmd.env("TERM", "xterm-256color");
    let mut child = pty.slave.spawn_command(cmd).context("ssh isn't installed on this machine")?;
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader()?;
    let mut writer = pty.master.take_writer()?;

    // The pseudo-terminal is blocking I/O: a thread each way.
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n @ 1..) = reader.read(&mut buf) {
            if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let (in_tx, in_rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        while let Ok(b) = in_rx.recv() {
            if writer.write_all(&b).and_then(|_| writer.flush()).is_err() {
                break;
            }
        }
    });

    // Text frames are keystrokes; a binary frame is a resize, [cols, rows].
    loop {
        tokio::select! {
            out = out_rx.recv() => match out {
                Some(b) => ws.send(Message::Binary(b.into())).await?,
                None => break,
            },
            msg = ws.recv() => match msg {
                Some(Ok(Message::Text(t))) => {
                    let _ = in_tx.send(t.as_bytes().to_vec());
                }
                Some(Ok(Message::Binary(b))) => {
                    if let Ok([cols, rows]) = serde_json::from_slice::<[u16; 2]>(&b) {
                        let _ = pty.master.resize(size(cols.clamp(10, 500), rows.clamp(3, 200)));
                    }
                }
                Some(Ok(_)) => {}
                _ => break,
            },
        }
    }
    let _ = child.kill();
    Ok(())
}
