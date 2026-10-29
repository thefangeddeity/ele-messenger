//! ELE Messenger desktop app. Shows the ELE Mesh (one shared group for the
//! ELE machines around you) and chats on it through the local API, running
//! its own node if this machine has no elemesh service.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod theme;

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
        viewport: egui::ViewportBuilder::default().with_title("ELE Messenger").with_inner_size([920.0, 620.0]).with_min_inner_size([560.0, 380.0]),
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
    ctx: egui::Context,
    light: bool,
    nick: String,
    room: String,
    new_room: String,
    draft: String,
    passphrase: String,
    joining: Arc<AtomicBool>,
    join_error: Arc<Mutex<Option<String>>>,
    send_error: Arc<Mutex<Option<String>>>,
    /// Menu: the machine-name box, the new-passphrase box, and the answer to
    /// the last thing the menu did.
    machine_name: String,
    new_passphrase: String,
    confirm_clear_all: bool,
    menu_note: Arc<Mutex<Option<String>>>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let get = |k: &str, d: &str| cc.storage.and_then(|s| s.get_string(k)).unwrap_or_else(|| d.to_string());
        let app = Self {
            backend: Arc::new(Mutex::new(backend::Backend::new())),
            mode: Arc::new(Mutex::new(Mode::Starting)),
            status: Default::default(),
            rooms: Default::default(),
            feed: Default::default(),
            generation: Default::default(),
            ctx,
            light: get("light", "") == "true",
            nick: get("nick", ""),
            room: get("room", "home"),
            new_room: String::new(),
            draft: String::new(),
            passphrase: String::new(),
            joining: Default::default(),
            join_error: Default::default(),
            send_error: Default::default(),
            machine_name: String::new(),
            new_passphrase: String::new(),
            confirm_clear_all: false,
            menu_note: Default::default(),
        };
        theme::apply(&app.ctx, app.light);
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
                            for m in msgs {
                                // Deletes and clears change what is shown;
                                // they are never shown themselves.
                                match &m.control {
                                    Some(backend::Control::Delete { author, seq }) => {
                                        f.messages.retain(|x| !(x.author == *author && x.seq == *seq))
                                    }
                                    Some(backend::Control::Clear) => f.messages.retain(|x| x.ts_ms > m.ts_ms),
                                    None => f.messages.push(m),
                                }
                            }
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
        let (joining, err, ctx) = (self.joining.clone(), self.join_error.clone(), self.ctx.clone());
        joining.store(true, Ordering::SeqCst);
        std::thread::spawn(move || {
            *err.lock().unwrap() = backend::set_passphrase(&pass).err().map(|e| format!("{e:#}"));
            joining.store(false, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }

    /// Runs a menu action off the UI thread and shows how it went.
    fn menu_action(&self, done: &'static str, f: impl FnOnce() -> anyhow::Result<()> + Send + 'static) {
        let (note, ctx) = (self.menu_note.clone(), self.ctx.clone());
        *note.lock().unwrap() = Some("working…".into());
        std::thread::spawn(move || {
            *note.lock().unwrap() = Some(match f() {
                Ok(()) => done.to_string(),
                Err(e) => format!("{e:#}"),
            });
            ctx.request_repaint();
        });
    }

    /// The same menu as the mesh page.
    fn menu(&mut self, ui: &mut egui::Ui, status: Option<&Status>) {
        let joined = status.map(|s| s.joined).unwrap_or(false);
        ui.set_min_width(300.0);

        ui.strong("This machine");
        if let Some(st) = status {
            ui.label(egui::RichText::new(format!("id {}", short(&st.node))).small().weak());
        }
        ui.horizontal(|ui| {
            let hint = status.map(|s| s.name.clone()).unwrap_or_default();
            ui.add(egui::TextEdit::singleline(&mut self.machine_name).hint_text(hint).desired_width(170.0));
            if ui.button("Rename").on_hover_text("What other machines and your messages show; empty goes back to the computer's name").clicked() {
                let name = std::mem::take(&mut self.machine_name);
                self.menu_action("renamed", move || backend::set_name(&name).map(|_| ()));
            }
        });
        ui.separator();

        ui.strong(if joined { "Mesh passphrase" } else { "Join the mesh" });
        ui.label(egui::RichText::new("The same on every machine in your group. Machines with different ones ignore each other.").small().weak());
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.new_passphrase).password(true).hint_text("new passphrase").desired_width(170.0));
            let ok = self.new_passphrase.trim().len() >= 8;
            if ui.add_enabled(ok, egui::Button::new(if joined { "Change" } else { "Join" })).clicked() {
                let p = std::mem::take(&mut self.new_passphrase);
                self.menu_action("passphrase saved; the mesh service restarted with it", move || backend::set_passphrase(&p));
            }
        });
        ui.separator();

        ui.strong(format!("This room: {}", self.room));
        if ui.button("Clear for me").on_hover_text("Hides this room's messages on this machine only").clicked() {
            let room = self.room.clone();
            self.menu_action("cleared on this machine", move || backend::clear(&room, false));
            self.follow(self.room.clone());
        }
        if !self.confirm_clear_all {
            if ui.button("Clear for everyone…").on_hover_text("Removes this room's messages on every machine in the mesh").clicked() {
                self.confirm_clear_all = true;
            }
        } else {
            ui.horizontal(|ui| {
                ui.label("Every machine loses this room's messages.");
                if ui.button("Clear").clicked() {
                    let room = self.room.clone();
                    self.menu_action("cleared for everyone", move || backend::clear(&room, true));
                    self.confirm_clear_all = false;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_clear_all = false;
                }
            });
        }
        ui.label(egui::RichText::new("To delete one of your own messages, right-click it.").small().weak());
        ui.separator();

        ui.strong("Appearance");
        ui.horizontal(|ui| {
            if ui.selectable_label(!self.light, "dark").clicked() && self.light {
                self.light = false;
                theme::apply(ui.ctx(), false);
            }
            if ui.selectable_label(self.light, "light").clicked() && !self.light {
                self.light = true;
                theme::apply(ui.ctx(), true);
            }
        });
        ui.separator();

        ui.strong("About");
        ui.label(format!("ELE Messenger {}", env!("CARGO_PKG_VERSION")));
        if let Some(st) = status.filter(|s| !s.version.is_empty()) {
            ui.label(egui::RichText::new(format!("mesh service {}", st.version)).small().weak());
        }
        if let Some(n) = self.menu_note.lock().unwrap().as_ref() {
            ui.separator();
            ui.label(n);
        }
    }
}

/// One row per ELE machine, merging its links and what we hear of it.
struct Machine {
    id: Option<String>,
    linked: Vec<String>,
    heard: Vec<String>,
    seen_s: u64,
    ips: Vec<String>,
    name: Option<String>,
}

fn machines(st: &Status) -> Vec<Machine> {
    let mut by: BTreeMap<String, Machine> = BTreeMap::new();
    let ip_of = |via: &str| via.rsplit_once(':').map(|(h, _)| h).unwrap_or(via).trim_matches(['[', ']']).to_string();
    for l in &st.links {
        let m = by.entry(l.peer.clone()).or_insert_with(|| Machine { id: Some(l.peer.clone()), linked: vec![], heard: vec![], seen_s: l.heard_s, ips: vec![], name: None });
        m.linked.push(l.transport.clone());
        m.name = m.name.take().or(l.name.clone());
        m.seen_s = m.seen_s.min(l.heard_s);
        if l.transport == "lan" {
            m.ips.push(ip_of(&l.via));
        }
    }
    for n in &st.nearby {
        let key = n.peer.clone().unwrap_or_else(|| format!("?{}", n.via));
        let m = by.entry(key).or_insert_with(|| Machine { id: n.peer.clone(), linked: vec![], heard: vec![], seen_s: n.seen_s, ips: vec![], name: None });
        m.heard.push(n.transport.clone());
        m.name = m.name.take().or(n.name.clone());
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
        storage.set_string("light", self.light.to_string());
    }

    fn on_exit(&mut self) {
        self.backend.lock().unwrap().shutdown();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let mode = *self.mode.lock().unwrap();
        let status = self.status.lock().unwrap().clone();
        let list = status.as_ref().map(machines).unwrap_or_default();

        egui::Panel::top("head").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.menu_button("☰", |ui| self.menu(ui, status.as_ref())).response.on_hover_text("Menu");
                ui.heading("ELE Mesh");
                let joined = status.as_ref().map(|s| s.joined).unwrap_or(true);
                let (chip, tip) = match mode {
                    Mode::Starting => ("starting…", "Looking for the mesh service on this computer"),
                    _ if !joined => ("not joined", "Listening for ELE machines; enter the mesh passphrase to join"),
                    Mode::Daemon => ("joined", "The mesh service runs on this computer; the app shows it"),
                    Mode::Builtin => ("joined", "The app runs the mesh itself while it is open"),
                };
                ui.label(egui::RichText::new(chip).weak()).on_hover_text(tip);
                if let Some(st) = &status {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(if st.name.is_empty() { format!("this machine {}", short(&st.node)) } else { format!("this machine: {} ({})", st.name, short(&st.node)) }).weak().monospace());
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
            egui::ScrollArea::vertical().show(ui, |ui| self.around(ui, mode, &list));
        });

        let joined = status.as_ref().map(|s| s.joined);
        egui::CentralPanel::default().show(ui, |ui| match (mode, joined) {
            (Mode::Starting, _) | (_, None) => {
                ui.centered_and_justified(|ui| ui.spinner());
            }
            (_, Some(false)) => self.join_card(ui, &list),
            (_, Some(true)) => self.chat(ui),
        });
    }
}

impl App {
    fn around(&self, ui: &mut egui::Ui, mode: Mode, list: &[Machine]) {
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
            ui.group(|ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let t = theme::tokens(self.light);
                    let (dot, colour) = if m.linked.is_empty() { ("○", t.text_muted) } else { ("●", t.success) };
                    // The name it gave over a link, else the start of its id.
                    let id = m.id.as_deref().map(short);
                    let name = m.name.clone().or(id.clone()).unwrap_or_else(|| "ELE machine (unnamed)".into());
                    ui.label(egui::RichText::new(dot).color(colour));
                    ui.label(egui::RichText::new(&name).strong());
                    if let Some(id) = id.filter(|i| *i != name) {
                        ui.label(egui::RichText::new(id).small().weak());
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
    }

    fn join_card(&mut self, ui: &mut egui::Ui, list: &[Machine]) {
        ui.add_space(30.0);
        ui.vertical_centered(|ui| {
            ui.set_max_width(ui.available_width().min(440.0));
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
            let default = self.status.lock().unwrap().as_ref().map(|s| s.name.clone()).filter(|n| !n.is_empty());
            let hint = default.unwrap_or_else(|| "shown to others".into());
            ui.add(egui::TextEdit::singleline(&mut self.nick).hint_text(hint).desired_width(120.0))
                .on_hover_text("Left empty, your messages carry this machine's name");
        });
        ui.separator();

        let bottom = 40.0;
        let me = self.status.lock().unwrap().as_ref().map(|s| s.node.clone()).unwrap_or_default();
        let mut delete: Option<u64> = None;
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
                    let row = ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(time_of(m.ts_ms)).weak().monospace());
                        ui.label(egui::RichText::new(who).strong()).on_hover_text(format!("from machine {}", short(&m.author)));
                        ui.label(&m.text);
                    });
                    if m.author == me {
                        row.response.interact(egui::Sense::click()).context_menu(|ui| {
                            if ui.button("Delete for everyone").clicked() {
                                delete = Some(m.seq);
                                ui.close();
                            }
                        });
                    }
                }
            });
        drop(feed);
        if let Some(seq) = delete {
            let room = self.room.clone();
            self.menu_action("message deleted", move || backend::delete(&room, seq));
        }

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
