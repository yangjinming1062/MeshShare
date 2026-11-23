use super::*;
use std::net::{IpAddr, Ipv4Addr};

pub(crate) fn is_tailscale_ipv4(address: Ipv4Addr) -> bool {
    (u32::from(address) & 0xffc00000) == 0x64400000
}

fn is_network_ipv4(address: Ipv4Addr) -> bool {
    address.is_private() || address.is_link_local() || is_tailscale_ipv4(address)
}

pub(crate) fn is_allowed_peer_address(address: IpAddr) -> bool {
    matches!(address, IpAddr::V4(ip) if ip.is_loopback() || is_network_ipv4(ip))
}

pub(crate) fn node_http_client() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PeerEndpointSource {
    Manual,
    Mdns,
    #[cfg(not(target_os = "android"))]
    Tailscale,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PeerEndpoint {
    pub source: PeerEndpointSource,
    pub url: String,
}

impl NodeRuntime {
    /// The first endpoint starts a dialer. Later candidates update its watch channel.
    pub(crate) fn offer_peer_endpoint(
        &self,
        node_id: &str,
        endpoint: PeerEndpoint,
    ) -> Option<watch::Receiver<Vec<PeerEndpoint>>> {
        let mut dials = self.peer_dials.lock();
        if self.stopping.load(Ordering::SeqCst) {
            return None;
        }
        if let Some(sender) = dials.get(node_id).filter(|sender| !sender.is_closed()) {
            sender.send_if_modified(|endpoints| {
                if endpoints.contains(&endpoint) {
                    return false;
                }
                if matches!(
                    endpoint.source,
                    PeerEndpointSource::Manual | PeerEndpointSource::Mdns
                ) {
                    endpoints.retain(|previous| previous.source != endpoint.source);
                }
                endpoints.push(endpoint);
                true
            });
            return None;
        }
        let (sender, receiver) = watch::channel(vec![endpoint]);
        dials.insert(node_id.to_string(), sender);
        Some(receiver)
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn forget_tailscale_endpoint(&self, node_id: &str, url: &str) {
        if let Some(sender) = self.peer_dials.lock().get(node_id) {
            sender.send_if_modified(|endpoints| {
                let count = endpoints.len();
                endpoints.retain(|endpoint| {
                    endpoint.source != PeerEndpointSource::Tailscale || endpoint.url != url
                });
                endpoints.len() != count
            });
        }
    }

    /// Retiring under the registration lock lets a concurrent discovery reuse the dialer
    /// or start its replacement, even if it arrives just as the last candidate expires.
    pub(crate) fn retire_peer_dialer(&self, node_id: &str) -> bool {
        let mut dials = self.peer_dials.lock();
        if !self.stopping.load(Ordering::SeqCst)
            && dials
                .get(node_id)
                .is_some_and(|sender| !sender.borrow().is_empty())
        {
            return false;
        }
        dials.remove(node_id);
        true
    }
}

pub(crate) fn normalize_peer_http_base(raw: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    // 完整主机名允许带末尾点（如 device.local.），校验时去掉，连接地址保持原样。
    let host = url.host_str()?.trim_end_matches('.');
    let allowed_host = host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".local")
        || host.parse().is_ok_and(is_allowed_peer_address);
    if !allowed_host {
        return None;
    }
    url.set_path("");
    Some(url.to_string().trim_end_matches('/').to_string())
}

pub(super) fn http_base_from_peer_endpoint(raw: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(raw).ok()?;
    let scheme = match url.scheme() {
        "ws" => "http",
        "wss" => "https",
        _ => return None,
    };
    url.set_scheme(scheme).ok()?;
    if url.path() != "/api/v1/ws" {
        return None;
    }
    url.set_path("");
    normalize_peer_http_base(url.as_str())
}

#[cfg(any(test, all(feature = "gui", not(target_os = "android"))))]
pub(super) fn mobile_ca_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "CA 地址无效".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("CA 地址只支持 HTTP 或 HTTPS".to_string());
    }
    let host = url.host_str().unwrap_or_default();
    if !host.parse().is_ok_and(is_allowed_peer_address) {
        return Err("CA 地址必须是节点的私网 IPv4 地址".to_string());
    }
    Ok(url)
}

pub(super) fn is_cellular_interface(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    // CLAT 把蜂窝接口包装成 v4-/v6- 前缀的同名接口，判定前先剥掉包装。
    // 宁可放过未知的运营商网卡名，也不用白名单漏掉手机 Wi-Fi 导致节点不可达。
    let name = name
        .strip_prefix("v4-")
        .or_else(|| name.strip_prefix("v6-"))
        .unwrap_or(&name);
    ["rmnet", "ccmni", "radio", "clat", "ppp", "pdp", "wwan"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

pub(super) fn is_android_vpn_interface(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if name.starts_with("tailscale") {
        return false;
    }
    ["tun", "tap", "utun", "wg", "wireguard", "vpn", "ipsec"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// 物理以太网和无线地址更可能是对端可达的入口；虚拟网卡与蜂窝地址排在后面，
/// 让公布顺序与手机、桌面互相直连时实际能用的地址一致。
pub(super) fn interface_rank(name: &str) -> u8 {
    let name = name.to_ascii_lowercase();
    if is_cellular_interface(&name) {
        return 2;
    }
    if [
        "vethernet",
        "veth",
        "vmnet",
        "docker",
        "bridge",
        "br-",
        "utun",
        "tun",
        "tap",
        "wg",
        "wireguard",
        "vpn",
        "ipsec",
        "tailscale",
        "zt",
        "dummy",
        "ifb",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
    {
        return 2;
    }
    if ["eth", "en", "wl", "wlan", "wi-fi", "ethernet"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        return 0;
    }
    1
}

pub(super) fn interface_names() -> HashMap<Ipv4Addr, String> {
    let mut names = HashMap::new();
    match if_addrs::get_if_addrs() {
        Ok(interfaces) => {
            for interface in interfaces {
                if let IpAddr::V4(address) = interface.ip() {
                    names.entry(address).or_insert(interface.name);
                }
            }
        }
        Err(error) => eprintln!("读取本机网卡名称失败：{error}"),
    }
    names
}

pub(super) fn network_addresses_for_bound(
    port: u16,
    bound: &BTreeSet<Ipv4Addr>,
) -> Vec<AddressInfo> {
    network_addresses(port, bound, &interface_names())
}

pub(super) fn network_addresses(
    port: u16,
    bound: &BTreeSet<Ipv4Addr>,
    names: &HashMap<Ipv4Addr, String>,
) -> Vec<AddressInfo> {
    let mut addresses = bound
        .iter()
        .copied()
        .filter(|address| is_network_ipv4(*address))
        .map(|address| {
            let kind = if is_tailscale_ipv4(address) {
                "tailscale"
            } else if address.is_private() {
                "lan"
            } else {
                "network"
            };
            AddressInfo {
                address: address.to_string(),
                url: format!("http://{address}:{port}"),
                label: address.to_string(),
                kind: kind.to_string(),
            }
        })
        .collect::<Vec<_>>();
    addresses.sort_by_key(|address| {
        let kind_rank = match address.kind.as_str() {
            "lan" => 0,
            "tailscale" => 1,
            _ => 2,
        };
        let parsed = address.address.parse::<Ipv4Addr>().ok();
        let interface = parsed
            .and_then(|ip| names.get(&ip).map(|name| interface_rank(name)))
            .unwrap_or(1);
        (
            kind_rank,
            interface,
            parsed.map(u32::from).unwrap_or(u32::MAX),
        )
    });
    addresses
}

pub fn bind_addresses() -> BTreeSet<Ipv4Addr> {
    let mut addresses = BTreeSet::from([Ipv4Addr::LOCALHOST]);
    match if_addrs::get_if_addrs() {
        Ok(interfaces) => {
            addresses.extend(bindable_addresses(
                NodeProfile::current(),
                interfaces.into_iter().map(|interface| {
                    let ip = interface.ip();
                    (interface.name, ip)
                }),
            ));
        }
        Err(error) => eprintln!("读取本机网卡失败：{error}"),
    }
    addresses
}

pub(super) fn bindable_addresses(
    profile: NodeProfile,
    interfaces: impl IntoIterator<Item = (String, IpAddr)>,
) -> BTreeSet<Ipv4Addr> {
    interfaces
        .into_iter()
        // 蜂窝地址在局域网内没有可达意义，Android 上也不应让节点绑定到运营商地址。
        .filter(|(name, _)| !(profile.is_android() && is_cellular_interface(name)))
        // Android 原生插件会把进程 socket 固定到 Wi-Fi/以太网；VPN 地址不能在该网络上监听。
        .filter(|(name, _)| !(profile.is_android() && is_android_vpn_interface(name)))
        .filter_map(|(_, ip)| match ip {
            IpAddr::V4(address) if is_network_ipv4(address) => Some(address),
            _ => None,
        })
        .collect()
}

impl NodeRuntime {
    pub fn set_listening_addresses(
        &self,
        http: &BTreeSet<Ipv4Addr>,
        https: impl Iterator<Item = Ipv4Addr>,
        https_port: u16,
    ) {
        let mut node = self.node.write();
        node.addresses = network_addresses_for_bound(node.port, http);
        node.mobile_addresses = https
            .filter(|ip| http.contains(ip) && is_network_ipv4(*ip))
            .map(|ip| AddressInfo {
                address: ip.to_string(),
                url: format!("https://{ip}:{https_port}"),
                label: ip.to_string(),
                kind: "mobile_https".to_string(),
            })
            .collect();
    }
}
