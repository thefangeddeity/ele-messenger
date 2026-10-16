//! ELE servers (server.py) announce themselves over mDNS as
//! `_ele-messenger._tcp`. This keeps the current list.

use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const SERVICE: &str = "_ele-messenger._tcp.local.";

#[derive(Clone)]
pub struct Server {
    pub name: String,
    pub host: String,
    pub addrs: Vec<String>,
    pub port: u16,
    pub scheme: String,
}

impl Server {
    /// The server's web page, by address rather than .local name, which not
    /// every machine resolves.
    pub fn url(&self) -> String {
        let host = self
            .addrs
            .iter()
            .find(|a| !a.contains(':') && !a.starts_with("127.") && !a.starts_with("100."))
            .or(self.addrs.first())
            .cloned()
            .unwrap_or_else(|| self.host.clone());
        let host = if host.contains(':') { format!("[{host}]") } else { host };
        let default = if self.scheme == "https" { 443 } else { 80 };
        if self.port == default { format!("{}://{host}/", self.scheme) } else { format!("{}://{host}:{}/", self.scheme, self.port) }
    }
}

pub type Servers = Arc<Mutex<BTreeMap<String, Server>>>;

/// Browses in the background; `changed` runs after every update.
pub fn browse(changed: impl Fn() + Send + 'static) -> Servers {
    let servers: Servers = Default::default();
    let out = servers.clone();
    std::thread::spawn(move || {
        let mdns = match ServiceDaemon::new() {
            Ok(m) => m,
            Err(e) => return tracing::warn!("mdns: {e}"),
        };
        let rx = match mdns.browse(SERVICE) {
            Ok(rx) => rx,
            Err(e) => return tracing::warn!("mdns browse: {e}"),
        };
        while let Ok(ev) = rx.recv() {
            match ev {
                ServiceEvent::ServiceResolved(info) => {
                    let name = info.fullname.trim_end_matches(SERVICE).trim_end_matches('.').replace("\\032", " ");
                    let txt = |k: &str| info.txt_properties.get_property_val_str(k).map(str::to_string);
                    let port = info.port;
                    let scheme = txt("scheme").unwrap_or_else(|| if port == 443 { "https".into() } else { "http".into() });
                    // Answers arrive per interface and address family; keep them all.
                    // Link-local IPv6 needs a scope id a URL can't carry, so skip it.
                    let mut list = out.lock().unwrap();
                    let mut addrs = list.get(&info.fullname).map(|s| s.addrs.clone()).unwrap_or_default();
                    for a in info.addresses.iter().map(|a| a.to_ip_addr()) {
                        let link_local = matches!(a, std::net::IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80);
                        if !link_local && !addrs.contains(&a.to_string()) {
                            addrs.push(a.to_string());
                        }
                    }
                    addrs.sort();
                    let s = Server { name, host: info.host.trim_end_matches('.').to_string(), addrs, port, scheme };
                    list.insert(info.fullname.clone(), s);
                    drop(list);
                    changed();
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    out.lock().unwrap().remove(&fullname);
                    changed();
                }
                _ => {}
            }
        }
    });
    servers
}
