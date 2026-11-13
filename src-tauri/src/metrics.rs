use crate::clock::now_millis;
use crate::model::{GpuMemoryKind, GpuMetrics, HardwareMetrics};
use crate::runtime::NodeRuntime;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, Networks, RefreshKind, System};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

pub fn start(runtime: Arc<NodeRuntime>) {
    thread::Builder::new()
        .name("meshshare-metrics".to_string())
        .spawn(move || {
            let mut collector = Collector::new(&runtime.node_id());
            while !runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                let started = Instant::now();
                let snapshot = collector.sample();
                runtime.set_local_metrics(snapshot.clone());
                runtime.publish_metrics(snapshot);
                let elapsed = started.elapsed();
                if elapsed < SAMPLE_INTERVAL {
                    thread::sleep(SAMPLE_INTERVAL - elapsed);
                }
            }
        })
        .expect("metrics collector thread");
}

struct Collector {
    node_id: String,
    system: System,
    networks: Networks,
    previous_network: Option<(Instant, u64, u64)>,
    cpu_ready: bool,
    gpus: GpuProbe,
}

impl Collector {
    fn new(node_id: &str) -> Self {
        let system = System::new_with_specifics(
            RefreshKind::new()
                .with_cpu(CpuRefreshKind::everything())
                .with_memory(MemoryRefreshKind::everything()),
        );
        Self {
            node_id: node_id.to_string(),
            system,
            networks: Networks::new_with_refreshed_list(),
            previous_network: None,
            cpu_ready: false,
            gpus: GpuProbe::new(),
        }
    }

    fn sample(&mut self) -> HardwareMetrics {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.networks.refresh_list();
        self.networks.refresh();

        let now = Instant::now();
        let (received, transmitted) = network_totals(&self.networks);
        let (network_down, network_up) = self
            .previous_network
            .map(|(previous_at, previous_received, previous_transmitted)| {
                let elapsed = now.duration_since(previous_at).as_secs_f64();
                if elapsed <= 0.0 {
                    return (None, None);
                }
                (
                    rate(previous_received, received, elapsed),
                    rate(previous_transmitted, transmitted, elapsed),
                )
            })
            .unwrap_or((None, None));
        self.previous_network = Some((now, received, transmitted));

        let cpu_usage_percent = if self.cpu_ready {
            Some(self.system.global_cpu_usage().clamp(0.0, 100.0))
        } else {
            self.cpu_ready = true;
            None
        };
        let mut gpus = self.gpus.sample();
        for gpu in &mut gpus {
            if gpu.memory_kind == GpuMemoryKind::Unified {
                gpu.memory_used_bytes = Some(self.system.used_memory());
                gpu.memory_total_bytes = Some(self.system.total_memory());
            }
        }

        HardwareMetrics {
            node_id: self.node_id.clone(),
            sampled_at_ms: now_millis(),
            cpu_usage_percent,
            memory_used_bytes: Some(self.system.used_memory()),
            memory_total_bytes: Some(self.system.total_memory()),
            network_down_bytes_per_sec: network_down,
            network_up_bytes_per_sec: network_up,
            gpus,
        }
    }
}

fn rate(previous: u64, current: u64, elapsed_seconds: f64) -> Option<u64> {
    if current < previous || elapsed_seconds <= 0.0 {
        return None;
    }
    Some(((current - previous) as f64 / elapsed_seconds).round() as u64)
}

fn network_totals(networks: &Networks) -> (u64, u64) {
    networks
        .iter()
        .filter(|(name, _)| !is_loopback_interface(name))
        .fold((0u64, 0u64), |(received, transmitted), (_, data)| {
            (
                received.saturating_add(data.total_received()),
                transmitted.saturating_add(data.total_transmitted()),
            )
        })
}

fn is_loopback_interface(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    normalized == "lo"
        || normalized.starts_with("lo0")
        || normalized.contains("loopback")
        || normalized.contains("loop-back")
}

struct GpuProbe {
    initialized: bool,
    static_gpus: Vec<GpuMetrics>,
}

impl GpuProbe {
    fn new() -> Self {
        Self {
            initialized: false,
            static_gpus: Vec::new(),
        }
    }

    fn sample(&mut self) -> Vec<GpuMetrics> {
        if !self.initialized {
            self.static_gpus = probe_static_gpus();
            self.initialized = true;
        }
        let mut gpus = probe_nvidia_smi();
        if gpus.is_empty() {
            #[cfg(target_os = "macos")]
            {
                gpus = self.static_gpus.clone();
            }
            #[cfg(not(target_os = "macos"))]
            {
                gpus = probe_platform_gpus();
            }
        } else {
            merge_gpu_metadata(&mut gpus, &self.static_gpus);
        }
        if gpus.is_empty() {
            self.static_gpus.clone()
        } else {
            gpus
        }
    }
}

fn merge_gpu_metadata(gpus: &mut [GpuMetrics], metadata: &[GpuMetrics]) {
    for gpu in gpus {
        if let Some(reference) = metadata.iter().find(|candidate| candidate.name == gpu.name) {
            if gpu.memory_total_bytes.is_none() {
                gpu.memory_total_bytes = reference.memory_total_bytes;
            }
            if gpu.memory_kind == GpuMemoryKind::Unknown {
                gpu.memory_kind = reference.memory_kind.clone();
            }
        }
    }
}

fn probe_nvidia_smi() -> Vec<GpuMetrics> {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,utilization.gpu,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_nvidia_line)
        .collect()
}

fn parse_nvidia_line(line: &str) -> Option<GpuMetrics> {
    let mut fields = line.split(',').map(str::trim);
    let name = fields.next()?.to_string();
    if name.is_empty() {
        return None;
    }
    let utilization_percent = parse_percent(fields.next()?);
    let memory_used_bytes = parse_mebibytes(fields.next()?);
    let memory_total_bytes = parse_mebibytes(fields.next()?);
    Some(GpuMetrics {
        name,
        utilization_percent,
        memory_used_bytes,
        memory_total_bytes,
        memory_kind: GpuMemoryKind::Dedicated,
        unavailable_reason: None,
    })
}

fn parse_percent(value: &str) -> Option<f32> {
    value
        .trim()
        .trim_end_matches('%')
        .parse::<f32>()
        .ok()
        .map(|value| value.clamp(0.0, 100.0))
}

fn parse_mebibytes(value: &str) -> Option<u64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| (value * 1024.0 * 1024.0).round() as u64)
}

fn probe_static_gpus() -> Vec<GpuMetrics> {
    #[cfg(target_os = "linux")]
    {
        return probe_linux_gpus();
    }
    #[cfg(target_os = "macos")]
    {
        return probe_macos_gpus();
    }
    #[cfg(target_os = "windows")]
    {
        return Vec::new();
    }
    #[allow(unreachable_code)]
    Vec::new()
}

#[cfg(target_os = "windows")]
fn probe_windows_gpus() -> Vec<GpuMetrics> {
    let script = r#"
$u = (Get-Counter '\GPU Engine(*)\Utilization Percentage' -ErrorAction SilentlyContinue).CounterSamples | Measure-Object -Property CookedValue -Sum | Select-Object -ExpandProperty Sum
$m = (Get-Counter '\GPU Adapter Memory(*)\Dedicated Usage' -ErrorAction SilentlyContinue).CounterSamples | Measure-Object -Property CookedValue -Sum | Select-Object -ExpandProperty Sum
$t = (Get-Counter '\GPU Adapter Memory(*)\Dedicated Limit' -ErrorAction SilentlyContinue).CounterSamples | Measure-Object -Property CookedValue -Sum | Select-Object -ExpandProperty Sum
Write-Output ("{0},{1},{2}" -f $u,$m,$t)
"#;
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(line) = stdout.lines().find(|line| line.contains(',')) else {
        return Vec::new();
    };
    let mut fields = line.split(',').map(str::trim);
    let utilization = fields.next().and_then(|value| value.parse::<f32>().ok());
    let used = fields.next().and_then(|value| value.parse::<f64>().ok());
    let total = fields.next().and_then(|value| value.parse::<f64>().ok());
    if utilization.is_none() && used.is_none() && total.is_none() {
        return Vec::new();
    }
    vec![GpuMetrics {
        name: "Windows GPU".to_string(),
        utilization_percent: utilization.map(|value| value.clamp(0.0, 100.0)),
        memory_used_bytes: used.map(|value| value.max(0.0) as u64),
        memory_total_bytes: total.map(|value| value.max(0.0) as u64),
        memory_kind: GpuMemoryKind::Dedicated,
        unavailable_reason: None,
    }]
}

#[cfg(not(target_os = "macos"))]
fn probe_platform_gpus() -> Vec<GpuMetrics> {
    #[cfg(target_os = "linux")]
    {
        return probe_linux_gpus();
    }
    #[cfg(target_os = "macos")]
    {
        return probe_macos_gpus();
    }
    #[cfg(target_os = "windows")]
    {
        return probe_windows_gpus();
    }
    #[allow(unreachable_code)]
    Vec::new()
}

#[cfg(target_os = "linux")]
fn probe_linux_gpus() -> Vec<GpuMetrics> {
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    let mut gpus = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let device = entry.path().join("device");
        let utilization = ["gpu_busy_percent", "gt_busy_percent"]
            .iter()
            .find_map(|file| read_percent(&device.join(file)));
        let memory_used = read_u64(&device.join("mem_info_vram_used"));
        let memory_total = read_u64(&device.join("mem_info_vram_total"));
        let driver = read_driver(&device).unwrap_or_else(|| name.clone());
        if utilization.is_none() && memory_used.is_none() && memory_total.is_none() {
            continue;
        }
        gpus.push(GpuMetrics {
            name: format!("{} ({})", driver, name),
            utilization_percent: utilization,
            memory_used_bytes: memory_used,
            memory_total_bytes: memory_total,
            memory_kind: if memory_total.is_some() {
                GpuMemoryKind::Dedicated
            } else {
                GpuMemoryKind::Unknown
            },
            unavailable_reason: if utilization.is_none() {
                Some("当前驱动未提供 GPU 利用率".to_string())
            } else {
                None
            },
        });
    }
    gpus
}

#[cfg(target_os = "linux")]
fn read_percent(path: &Path) -> Option<f32> {
    read_text(path).and_then(|value| parse_percent(&value))
}

#[cfg(target_os = "linux")]
fn read_u64(path: &Path) -> Option<u64> {
    read_text(path).and_then(|value| value.trim().parse::<u64>().ok())
}

#[cfg(target_os = "linux")]
fn read_driver(device: &Path) -> Option<String> {
    read_text(&device.join("uevent")).and_then(|contents| {
        contents.lines().find_map(|line| {
            line.strip_prefix("DRIVER=")
                .map(|value| value.trim().to_string())
        })
    })
}

#[cfg(target_os = "linux")]
fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(target_os = "macos")]
fn probe_macos_gpus() -> Vec<GpuMetrics> {
    let output = Command::new("system_profiler")
        .args(["SPDisplaysDataType", "-json"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    value
        .get("SPDisplaysDataType")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|gpu| {
            let name = gpu
                .get("sppci_model")
                .or_else(|| gpu.get("_name"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("GPU")
                .to_string();
            let total = gpu
                .get("spdisplays_vram")
                .and_then(serde_json::Value::as_str)
                .and_then(parse_human_memory);
            GpuMetrics {
                name,
                utilization_percent: None,
                memory_used_bytes: None,
                memory_total_bytes: total,
                memory_kind: if total.is_some() {
                    GpuMemoryKind::Dedicated
                } else {
                    GpuMemoryKind::Unified
                },
                unavailable_reason: Some("macOS 当前未提供无特权 GPU 利用率采集".to_string()),
            }
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn parse_human_memory(value: &str) -> Option<u64> {
    let mut parts = value.split_whitespace();
    let amount = parts.next()?.parse::<f64>().ok()?;
    let unit = parts.next().unwrap_or("GB").to_ascii_lowercase();
    let multiplier = match unit.as_str() {
        "kb" => 1024.0,
        "mb" => 1024.0 * 1024.0,
        "gb" => 1024.0 * 1024.0 * 1024.0,
        "tb" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => 1.0,
    };
    Some((amount * multiplier).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::{is_loopback_interface, network_totals, parse_nvidia_line, rate};
    use sysinfo::Networks;

    #[test]
    fn rate_handles_counter_reset_and_elapsed_time() {
        assert_eq!(rate(100, 300, 2.0), Some(100));
        assert_eq!(rate(300, 100, 2.0), None);
        assert_eq!(rate(100, 300, 0.0), None);
    }

    #[test]
    fn loopback_interfaces_are_excluded() {
        assert!(is_loopback_interface("lo0"));
        assert!(is_loopback_interface("Loopback Pseudo-Interface 1"));
        assert!(!is_loopback_interface("en0"));
    }

    #[test]
    fn gpu_parser_keeps_unavailable_values_empty() {
        let gpu = parse_nvidia_line("Test GPU, N/A, N/A, 2048").expect("parse gpu");
        assert_eq!(gpu.utilization_percent, None);
        assert_eq!(gpu.memory_used_bytes, None);
        assert_eq!(gpu.memory_total_bytes, Some(2048 * 1024 * 1024));
    }

    #[test]
    fn network_totals_are_empty_for_new_collection() {
        let networks = Networks::new();
        assert_eq!(network_totals(&networks), (0, 0));
    }
}
