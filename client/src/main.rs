//! ELE desktop client. Shows the ELE Mesh (one shared group for the ELE
//! machines around you) next to the ELE servers on the network, and chats on
//! the mesh through the local API, running its own node if this machine has
//! no elemesh daemon.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod servers;

use backend::{Message, Mode, Status};
use eframe::egui;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const POLL: Duration = Duration::from_secs(2);
const WAIT_S: u64 = 20;

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "elemesh=info,ele=info".into()))
        .init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("ELE").with_inner_size([920.0, 620.0]).with_min_inner_size([560.0, 380.0]),
        ..Default::default()
    };
    eframe::run_native("ELE", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[derive(Default)]
struct Feed {
    generation: u64,
    messages: Vec<Message>,
}

struct App {
    backend: Arc<Mutex<backend::Backend>>,
    mode: Arc<Mutex<Mode>>,
    status: Arc<Mutex<Option<Status>>>,
    rooms: Arc<Mutex<Vec<String>>>,
    feed: Arc<Mutex<Feed>>,
    generation: Arc<AtomicU64>,
    servers: servers::Servers,
    ctx: egui::Context,
    nick: String,
    room: String,
    new_room: String,
    draft: String,
    passphrase: String,
    joining: Arc<AtomicBool>,
    join_error: Arc<Mutex<Option<String>>>,
    send_error: Arc<Mutex<Option<String>>>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let get = |k: &str, d: &str| cc.storage.and_then(|s| s.get_string(k)).unwrap_or_else(|| d.to_string());
        let repaint = ctx.clone();
        let app = Self {
            backend: Arc::new(Mutex::new(backend::Backend::new())),
            mode: Arc::new(Mutex::new(Mode::Starting)),
            status: Default::default(),
            rooms: Default::default(),
            feed: Default::default(),
            generation: Default::default(),
            servers: servers::browse(move || repaint.request_repaint()),
            ctx,
            nick: get("nick", ""),
            room: get("room", "home"),
            new_room: String::new(),
            draft: String::new(),
            passphrase: String::new(),
            joining: Default::default(),
            join_error: Default::default(),
            send_error: Default::default(),
        };
        app.start_backend();
        app.start_status_poll();
        app.follow(app.room.clone());
        app
    }

    fn start_backend(&self) {
        let (b, mode, ctx) = (self.backend.clone(), self.mode.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let mut b = b.lock().unwrap();
            b.connect();
            *mode.lock().unwrap() = b.mode;
            ctx.request_repaint();
        });
    }

    fn start_status_poll(&self) {
        let (status, rooms, ctx) = (self.status.clone(), self.rooms.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            loop {
                *status.lock().unwrap() = backend::status().ok();
                if let Ok(r) = backend::rooms() {
                    *rooms.lock().unwrap() = r;
                }
                ctx.request_repaint();
                std::thread::sleep(POLL);
            }
        });
    }

    /// Follows one room; a room switch starts a new follower and the old one
    /// stops after its current wait.
    fn follow(&self, room: String) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut f = self.feed.lock().unwrap();
            *f = Feed { generation, messages: Vec::new() };
        }
        let (feed, current, ctx) = (self.feed.clone(), self.generation.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let mut after = 0;
            while current.load(Ordering::SeqCst) == generation {
                match backend::messages(&room, after, WAIT_S) {
                    Ok((cursor, msgs)) => {
                        let mut f = feed.lock().unwrap();
                        if f.generation != generation {
                            break;
                        }
                        after = cursor;
                        if !msgs.is_empty() {
                            f.messages.extend(msgs);
                            ctx.request_repaint();
                        }
                    }
                    Err(_) => std::thread::sleep(POLL),
                }
            }
        });
    }

    fn send(&mut self) {
        let text = self.draft.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.draft.clear();
        let (room, nick, err, ctx) = (self.room.clone(), self.nick.trim().to_string(), self.send_error.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            *err.lock().unwrap() = backend::post(&room, &nick, &text).err().map(|e| e.to_string());
            ctx.request_repaint();
        });
    }

    fn join(&mut self) {
        let pass = std::mem::take(&mut self.passphrase);
        let (b, mode, joining, err, ctx) =
            (self.backend.clone(), self.mode.clone(), self.joining.clone(), self.join_error.clone(), self.ctx.clone());
        joining.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            let mut b = b.lock().unwrap();
            *err.lock().unwrap() = b.join(&pass).err().map(|e| format!("{e:#}"));
            *mode.lock().unwrap() = b.mode;
            joining.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }
}

/// One row per ELE machine, merging its links and what we hear of it.
struct Machine {
    id: Option<String>,
    linked: Vec<String>,
    heard: Vec<String>,
    seen_s: u64,
    ips: Vec<String>,
}

fn machines(st: &Status) -> Vec<Machine> {
    let mut by: BTreeMap<String, Machine> = BTreeMap::new();
    let ip_of = |via: &str| via.rsplit_once(':').map(|(h, _)| h).unwrap_or(via).trim_matches(['[', ']']).to_string();
    for l in &st.links {
        let m = by.entry(l.peer.clone()).or_insert_with(|| Machine { id: Some(l.peer.clone()), linked: vec![], heard: vec![], seen_s: l.heard_s, ips: vec![] });
        m.linked.push(l.transport.clone());
        m.seen_s = m.seen_s.min(l.heard_s);
        if l.transport == "lan" {
            m.ips.push(ip_of(&l.via));
        }
    }
    for n in &st.nearby {
        let key = n.peer.clone().unwrap_or_else(|| format!("?{}", n.via));
        let m = by.entry(key).or_insert_with(|| Machine { id: n.peer.clone(), linked: vec![], heard: vec![], seen_s: n.seen_s, ips: vec![] });
        m.heard.push(n.transport.clone());
        m.seen_s = m.seen_s.min(n.seen_s);
        if n.transport == "lan" {
            m.ips.push(n.via.clone());
        }
    }
    let mut out: Vec<Machine> = by.into_values().collect();
    out.sort_by_key(|m| (m.linked.is_empty(), m.seen_s));
    out
}

fn transport_name(t: &str) -> &str {
    match t {
        "ble" => "Bluetooth",
        "lan" => "Wi-Fi/LAN",
        other => other,
    }
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

fn time_of(ts_ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_ms as i64)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default()
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("nick", self.nick.clone());
        storage.set_string("room", self.room.clone());
    }

    fn on_exit(&mut self) {
        self.backend.lock().unwrap().shutdown();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let mode = *self.mode.lock().unwrap();
        let status = self.status.lock().unwrap().clone();
        let servers: Vec<servers::Server> = self.servers.lock().unwrap().values().cloned().collect();
        let list = status.as_ref().map(machines).unwrap_or_default();

        egui::Panel::top("head").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.heading("ELE Mesh");
                let (chip, tip) = match mode {
                    Mode::Starting => ("starting…", "Looking for an ELE daemon on this computer"),
                    Mode::Daemon => ("this computer's ELE daemon", "elemesh runs on this computer; the client shows it"),
                    Mode::Builtin => ("built-in node", "No daemon here, so the client runs its own node"),
                    Mode::Scouting => ("not joined", "Listening for ELE machines; enter the mesh passphrase to join"),
                };
                ui.label(egui::RichText::new(chip).weak()).on_hover_text(tip);
                if let Some(st) = &status {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(format!("this machine {}", short(&st.node))).weak().monospace());
                    });
                }
            });
            ui.label(
                "One shared group for the ELE machines within reach of this one: over Bluetooth and Wi-Fi, \
                 no internet or server needed, and carried further by any machine in between. \
                 Everyone with the mesh passphrase reads every room.",
            );
            ui.add_space(6.0);
        });

        egui::Panel::left("around").resizable(true).default_size(290.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.around(ui, mode, &list, &servers));
        });

        egui::CentralPanel::default().show(ui, |ui| match mode {
            Mode::Starting => {
                ui.centered_and_justified(|ui| ui.spinner());
            }
            Mode::Scouting => self.join_card(ui, &list),
            Mode::Daemon | Mode::Builtin => self.chat(ui),
        });
    }
}

impl App {
    fn around(&self, ui: &mut egui::Ui, mode: Mode, list: &[Machine], servers: &[servers::Server]) {
        ui.add_space(4.0);
        ui.strong("Around you: ELE Mesh");
        ui.label(egui::RichText::new("Machines heard nearby. Linked ones share messages now.").small().weak());
        ui.add_space(4.0);
        if list.is_empty() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(if mode == Mode::Starting { "Starting…" } else { "No ELE machines around yet. Listening over Bluetooth and Wi-Fi…" });
            });
        }
        for m in list {
            let server = servers.iter().find(|s| m.ips.iter().any(|ip| s.addrs.contains(ip)));
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let dot = if m.linked.is_empty() { "○" } else { "●" };
                    let name = m.id.as_deref().map(short).unwrap_or_else(|| "ELE machine (unnamed)".into());
                    ui.label(egui::RichText::new(format!("{dot} {name}")).monospace().strong());
                    if server.is_some() {
                        ui.label(egui::RichText::new("also a server").small()).on_hover_text("This machine also runs an ELE server");
                    }
                });
                let mut ts: Vec<&str> = m.linked.iter().map(|t| transport_name(t)).collect();
                ts.dedup();
                let line = if m.linked.is_empty() {
                    let mut h: Vec<&str> = m.heard.iter().map(|t| transport_name(t)).collect();
                    h.dedup();
                    format!("heard over {} · {} s ago", h.join(", "), m.seen_s)
                } else {
                    format!("linked over {} · {} s ago", ts.join(", "), m.seen_s)
                };
                ui.label(egui::RichText::new(line).small());
            });
        }

        ui.add_space(14.0);
        ui.strong("ELE servers");
        ui.label(
            egui::RichText::new("The full chat platform: accounts, direct messages, groups, images. Works while the server and the network are up.")
                .small()
                .weak(),
        );
        ui.add_space(4.0);
        if servers.is_empty() {
            ui.label("No ELE servers around.");
        }
        for s in servers {
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.label(egui::RichText::new(&s.name).strong());
                let url = s.url();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&url).small().monospace());
                    if ui.button("Open").on_hover_text("Open this server's page in your browser").clicked() {
                        let _ = webbrowser::open(&url);
                    }
                });
            });
        }
    }

    fn join_card(&mut self, ui: &mut egui::Ui, list: &[Machine]) {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.set_max_width(440.0);
            ui.heading("Join the ELE Mesh");
            ui.add_space(8.0);
            let n = list.len();
            ui.label(match n {
                0 => "No ELE machines heard yet. You can still join: others will link to you when they come within reach.".to_string(),
                1 => "1 ELE machine is within reach.".to_string(),
                _ => format!("{n} ELE machines are within reach."),
            });
            ui.add_space(8.0);
            ui.label("Enter the mesh passphrase. It's the same on every machine in your group; anyone who has it can read every room.");
            ui.add_space(8.0);
            let busy = self.joining.load(Ordering::SeqCst);
            let edit = ui.add_enabled(!busy, egui::TextEdit::singleline(&mut self.passphrase).password(true).hint_text("mesh passphrase"));
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(6.0);
            let go = ui.add_enabled(!busy && self.passphrase.trim().len() >= 8, egui::Button::new("Join"));
            if busy {
                ui.spinner();
            }
            if (go.clicked() || enter) && !busy && self.passphrase.trim().len() >= 8 {
                self.join();
            }
            if let Some(e) = self.join_error.lock().unwrap().as_ref() {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
        });
    }

    fn chat(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Room");
            let rooms = self.rooms.lock().unwrap().clone();
            let mut chosen = self.room.clone();
            egui::ComboBox::from_id_salt("room").selected_text(&chosen).show_ui(ui, |ui| {
                for r in &rooms {
                    ui.selectable_value(&mut chosen, r.clone(), r);
                }
            });
            let add = ui.add(egui::TextEdit::singleline(&mut self.new_room).hint_text("new room").desired_width(110.0));
            if add.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.new_room.trim().is_empty() {
                chosen = std::mem::take(&mut self.new_room).trim().to_string();
            }
            if chosen != self.room {
                self.room = chosen;
                self.follow(self.room.clone());
            }
            ui.separator();
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut self.nick).hint_text("shown to others").desired_width(120.0));
        });
        ui.separator();

        let bottom = 40.0;
        let feed = self.feed.lock().unwrap();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .max_height(ui.available_height() - bottom)
            .show(ui, |ui| {
                if feed.messages.is_empty() {
                    ui.label(egui::RichText::new("No messages in this room yet.").weak());
                }
                for m in &feed.messages {
                    let who = if m.nick.is_empty() { short(&m.author) } else { m.nick.clone() };
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(time_of(m.ts_ms)).weak().monospace());
                        ui.label(egui::RichText::new(who).strong()).on_hover_text(format!("from machine {}", short(&m.author)));
                        ui.label(&m.text);
                    });
                }
            });
        drop(feed);

        ui.separator();
        ui.horizontal(|ui| {
            let edit = ui.add(egui::TextEdit::singleline(&mut self.draft).hint_text("Message").desired_width(ui.available_width() - 70.0));
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Send").clicked() || enter {
                self.send();
                edit.request_focus();
            }
        });
        if let Some(e) = self.send_error.lock().unwrap().as_ref() {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
    }
}
