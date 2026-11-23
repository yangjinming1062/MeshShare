use crate::clock::now_millis;
use crate::model::{DiskMetrics, GpuMemoryKind, GpuMetrics, HardwareMetrics, LoadAverage};
use crate::runtime::NodeRuntime;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::{Duration, Instant};
use sysinfo::{CpuRefreshKind, Disks, MemoryRefreshKind, Networks, RefreshKind, System};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const DISK_LIST_REFRESH_INTERVAL: u32 = 10;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// 安装版进程没有控制台，外部命令会各自新建控制台窗口，表现为反复闪出终端。
fn hidden_command(program: &str) -> Command {
    let command = Command::new(program);
    #[cfg(target_os = "windows")]
    let command = {
        use std::os::windows::process::CommandExt;
        let mut command = command;
        command.creation_flags(CREATE_NO_WINDOW);
        command
    };
    command
}

pub fn start(runtime: Arc<NodeRuntime>) {
    thread::Builder::new()
        .name("meshshare-metrics".to_string())
        .spawn(move || {
            let network_only = runtime.is_android_profile();
            let mut collector = Collector::new(&runtime.node_id(), network_only);
            while !runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                let started = Instant::now();
                runtime.publish_metrics(collector.sample());
                thread::sleep(SAMPLE_INTERVAL.saturating_sub(started.elapsed()));
            }
        })
        .expect("metrics collector thread");
}

struct Collector {
    node_id: String,
    networks: Networks,
    previous_network: Option<(Instant, u64, u64)>,
    desktop: Option<DesktopCollector>,
}

impl Collector {
    fn new(node_id: &str, network_only: bool) -> Self {
        Self {
            node_id: node_id.to_owned(),
            networks: Networks::new_with_refreshed_list(),
            previous_network: None,
            desktop: (!network_only).then(DesktopCollector::new),
        }
    }

    fn sample(&mut self) -> HardwareMetrics {
        self.networks.refresh_list();
        self.networks.refresh();
        let now = Instant::now();
        let (received, transmitted) = network_totals(&self.networks);
        let mut snapshot = HardwareMetrics {
            node_id: self.node_id.clone(),
            ..HardwareMetrics::default()
        };
        if let Some((at, rx, tx)) = self.previous_network.replace((now, received, transmitted)) {
            let elapsed = now.duration_since(at).as_secs_f64();
            snapshot.network_down_bytes_per_sec = rate(rx, received, elapsed);
            snapshot.network_up_bytes_per_sec = rate(tx, transmitted, elapsed);
        }
        if let Some(desktop) = &mut self.desktop {
            desktop.sample(&mut snapshot);
        }
        snapshot.sampled_at_ms = now_millis();
        snapshot
    }
}

struct DesktopCollector {
    system: System,
    cpu_ready: bool,
    disks: Disks,
    disk_samples_since_refresh: u32,
    static_gpus: OnceLock<Vec<GpuMetrics>>,
}

impl DesktopCollector {
    fn new() -> Self {
        Self {
            system: System::new_with_specifics(
                RefreshKind::new()
                    .with_cpu(CpuRefreshKind::everything())
                    .with_memory(MemoryRefreshKind::everything()),
            ),
            cpu_ready: false,
            disks: Disks::new(),
            disk_samples_since_refresh: 0,
            static_gpus: OnceLock::new(),
        }
    }

    fn sample(&mut self, metrics: &mut HardwareMetrics) {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.refresh_disks();
        let system = &self.system;
        if self.cpu_ready {
            metrics.cpu_usage_percent = Some(system.global_cpu_usage().clamp(0.0, 100.0));
        }
        self.cpu_ready = true;
        metrics.cpu_model = cpu_model(system);
        metrics.cpu_logical_cores = u32::try_from(system.cpus().len()).ok();
        metrics.cpu_physical_cores = system
            .physical_core_count()
            .and_then(|n| u32::try_from(n).ok());
        metrics.cpu_frequency_mhz = system
            .cpus()
            .iter()
            .map(|cpu| cpu.frequency())
            .max()
            .filter(|n| *n > 0);
        metrics.memory_used_bytes = Some(system.used_memory());
        metrics.memory_total_bytes = Some(system.total_memory());
        metrics.memory_available_bytes = Some(system.available_memory());
        metrics.swap_used_bytes = Some(system.used_swap());
        metrics.swap_total_bytes = Some(system.total_swap());
        metrics.load_average = system_load_average();
        metrics.uptime_seconds = Some(System::uptime());
        metrics.disks = collect_disks(&self.disks);
        metrics.gpus = collect_gpus(self.static_gpus.get_or_init(probe_static_gpus));
    }

    fn refresh_disks(&mut self) {
        if self.disk_samples_since_refresh == 0 {
            self.disks.refresh_list();
        } else {
            self.disks.refresh();
        }
        self.disk_samples_since_refresh =
            (self.disk_samples_since_refresh + 1) % DISK_LIST_REFRESH_INTERVAL;
    }
}

fn cpu_model(system: &System) -> Option<String> {
    system.cpus().iter().find_map(|cpu| {
        let value = if cpu.brand().trim().is_empty() {
            cpu.name().trim()
        } else {
            cpu.brand().trim()
        };
        (!value.is_empty()).then(|| value.to_string())
    })
}

#[cfg(target_os = "windows")]
fn system_load_average() -> Option<LoadAverage> {
    None
}

#[cfg(not(target_os = "windows"))]
fn system_load_average() -> Option<LoadAverage> {
    let value = System::load_average();
    [value.one, value.five, value.fifteen]
        .iter()
        .all(|item| item.is_finite() && *item >= 0.0)
        .then_some(LoadAverage {
            one: value.one,
            five: value.five,
            fifteen: value.fifteen,
        })
}

fn collect_disks(disks: &Disks) -> Vec<DiskMetrics> {
    disks
        .iter()
        .filter_map(|disk| {
            let total_bytes = disk.total_space();
            if total_bytes == 0 {
                return None;
            }
            let mount_point = disk.mount_point().to_string_lossy().trim().to_string();
            let name = disk.name().to_string_lossy().trim().to_string();
            Some(DiskMetrics {
                name: if name.is_empty() {
                    mount_point.clone()
                } else {
                    name
                },
                mount_point,
                file_system: disk.file_system().to_string_lossy().trim().to_string(),
                total_bytes,
                available_bytes: disk.available_space().min(total_bytes),
                removable: disk.is_removable(),
            })
        })
        .collect()
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

fn collect_gpus(metadata: &[GpuMetrics]) -> Vec<GpuMetrics> {
    let mut gpus = probe_nvidia_smi();
    #[cfg(target_os = "windows")]
    if gpus.is_empty() {
        gpus = probe_windows_gpus();
    }
    #[cfg(target_os = "linux")]
    if gpus.is_empty() {
        gpus = probe_linux_gpus();
    }
    if gpus.is_empty() {
        return metadata.to_vec();
    }
    for gpu in &mut gpus {
        if let Some(reference) = metadata.iter().find(|candidate| candidate.name == gpu.name) {
            if gpu.memory_total_bytes.is_none() {
                gpu.memory_total_bytes = reference.memory_total_bytes;
            }
            if gpu.memory_kind == GpuMemoryKind::Unknown {
                gpu.memory_kind = reference.memory_kind.clone();
            }
        }
    }
    gpus
}

fn probe_nvidia_smi() -> Vec<GpuMetrics> {
    let output = hidden_command("nvidia-smi")
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

#[cfg(target_os = "linux")]
use probe_linux_gpus as probe_static_gpus;
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn probe_static_gpus() -> Vec<GpuMetrics> {
    Vec::new()
}

#[cfg(target_os = "windows")]
const WINDOWS_GPU_FALLBACK_NAME: &str = "Windows GPU";

#[cfg(target_os = "windows")]
const WINDOWS_GPU_SCRIPT: &str = r#"
$ProgressPreference = 'SilentlyContinue'
$memory = Get-CimInstance -ClassName Win32_PerfFormattedData_GPUPerformanceCounters_GPUAdapterMemory -ErrorAction SilentlyContinue
$engines = Get-CimInstance -ClassName Win32_PerfFormattedData_GPUPerformanceCounters_GPUEngine -ErrorAction SilentlyContinue
$dedicated = 0
foreach ($item in $memory) { $dedicated = $dedicated + [long]$item.DedicatedUsage }
$utilization = 0
if ($engines) {
  foreach ($group in ($engines | Group-Object { $_.Name -replace '^pid_[0-9]+_', '' })) {
    $sum = ($group.Group | Measure-Object -Property UtilizationPercentage -Sum).Sum
    if ($sum -gt $utilization) { $utilization = $sum }
  }
}
$fields = @('', '')
if ($engines) { $fields[0] = [string][long]$utilization }
if ($memory) { $fields[1] = [string]$dedicated }
Write-Output ($fields -join ';')
"#;

// 性能计数器只有适配器 LUID 维度、没有每卡容量，Windows 因此聚合为一条记录。
#[cfg(target_os = "windows")]
fn probe_windows_gpus() -> Vec<GpuMetrics> {
    let output = hidden_command("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            WINDOWS_GPU_SCRIPT,
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some((utilization, dedicated)) = stdout.lines().find_map(parse_windows_gpu_line) else {
        return Vec::new();
    };
    let (name, dedicated_total) = windows_display_adapter();
    vec![GpuMetrics {
        name: name.unwrap_or_else(|| WINDOWS_GPU_FALLBACK_NAME.to_string()),
        utilization_percent: utilization,
        // 没有独立显存的适配器（核显）不报占用，界面保持空值。
        memory_used_bytes: (dedicated_total > 0).then_some(dedicated.unwrap_or_default()),
        memory_total_bytes: None,
        memory_kind: GpuMemoryKind::Unknown,
        unavailable_reason: None,
    }]
}

#[cfg(target_os = "windows")]
fn parse_windows_gpu_line(line: &str) -> Option<(Option<f32>, Option<u64>)> {
    let fields: Vec<&str> = line.trim().split(';').map(str::trim).collect();
    if fields.len() != 2 {
        return None;
    }
    let utilization = fields[0]
        .parse::<f32>()
        .ok()
        .map(|value| value.clamp(0.0, 100.0));
    let dedicated = fields[1].parse::<u64>().ok();
    if utilization.is_none() && dedicated.is_none() {
        return None;
    }
    Some((utilization, dedicated))
}

#[cfg(target_os = "windows")]
fn probe_static_gpus() -> Vec<GpuMetrics> {
    let (name, dedicated_total) = windows_display_adapter();
    vec![GpuMetrics {
        name: name.unwrap_or_else(|| WINDOWS_GPU_FALLBACK_NAME.to_string()),
        utilization_percent: None,
        memory_used_bytes: None,
        memory_total_bytes: (dedicated_total > 0).then_some(dedicated_total),
        memory_kind: if dedicated_total > 0 {
            GpuMemoryKind::Dedicated
        } else {
            GpuMemoryKind::Unknown
        },
        unavailable_reason: None,
    }]
}

// 独立显存容量取自显示类驱动键的 REG_QWORD；核显通常没有该值，表示没有自带显存。
#[cfg(target_os = "windows")]
fn windows_display_adapter() -> (Option<String>, u64) {
    const DISPLAY_CLASS_KEY: &str =
        r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let Ok(root) = windows_registry::LOCAL_MACHINE.open(DISPLAY_CLASS_KEY) else {
        return (None, 0);
    };
    let Ok(children) = root.keys() else {
        return (None, 0);
    };
    let mut total = 0u64;
    let mut largest = 0u64;
    let mut name = None;
    for child in children {
        let Ok(key) = root.open(child) else {
            continue;
        };
        let Ok(size) = key.get_u64("HardwareInformation.qwMemorySize") else {
            continue;
        };
        total = total.saturating_add(size);
        if size > largest {
            largest = size;
            name = key.get_string("DriverDesc").ok();
        }
    }
    (name, total)
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
        if utilization.is_none() && memory_used.is_none() && memory_total.is_none() {
            continue;
        }
        let slot = read_uevent_field(&device, "PCI_SLOT_NAME");
        let display_name = slot
            .as_deref()
            .and_then(|slot| read_linux_gpu_name(slot, memory_total))
            .or_else(|| read_driver(&device))
            .unwrap_or_else(|| name.clone());
        gpus.push(GpuMetrics {
            name: display_name,
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
fn read_linux_gpu_name(slot: &str, memory_total_bytes: Option<u64>) -> Option<String> {
    let slot = slot.strip_prefix("0000:").unwrap_or(slot);
    let output = hidden_command("lspci")
        .args(["-mm", "-s", slot])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let fields = parse_lspci_machine_fields(&String::from_utf8_lossy(&output.stdout))?;
    fields
        .get(2)
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| refine_linux_gpu_name(value.trim(), memory_total_bytes))
}

#[cfg(any(target_os = "linux", test))]
fn refine_linux_gpu_name(name: &str, memory_total_bytes: Option<u64>) -> String {
    // pci.ids 把消费级 Navi 31 显卡归在同一个设备 ID 下，用独立显存容量区分
    // 常见的 20 GiB XT 与 24 GiB XTX 版本，无需依赖可选的厂商工具。
    if name.contains("Navi 31") {
        if let Some(memory_total_bytes) = memory_total_bytes {
            let memory_gib =
                memory_total_bytes.saturating_add(512 * 1024 * 1024) / (1024 * 1024 * 1024);
            if memory_gib == 20 {
                return "Radeon RX 7900 XT".to_string();
            }
            if memory_gib == 24 {
                return "Radeon RX 7900 XTX".to_string();
            }
        }
    }
    name.to_string()
}

#[cfg(any(target_os = "linux", test))]
fn parse_lspci_machine_fields(output: &str) -> Option<Vec<String>> {
    let line = output.lines().find(|line| !line.trim().is_empty())?;
    let mut fields = Vec::new();
    let mut chars = line.chars();
    while let Some(character) = chars.next() {
        if character != '"' {
            continue;
        }
        let mut field = String::new();
        let mut escaped = false;
        for character in chars.by_ref() {
            if escaped {
                field.push(character);
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                break;
            } else {
                field.push(character);
            }
        }
        fields.push(field);
    }
    (!fields.is_empty()).then_some(fields)
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
    read_uevent_field(device, "DRIVER")
}

#[cfg(target_os = "linux")]
fn read_uevent_field(device: &Path, field: &str) -> Option<String> {
    read_text(&device.join("uevent")).and_then(|contents| {
        contents.lines().find_map(|line| {
            line.strip_prefix(field)
                .and_then(|rest| rest.strip_prefix('='))
                .map(|value| value.trim().to_string())
        })
    })
}

#[cfg(target_os = "linux")]
fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[cfg(target_os = "macos")]
fn probe_static_gpus() -> Vec<GpuMetrics> {
    let output = hidden_command("system_profiler")
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
    use super::{
        is_loopback_interface, network_totals, parse_lspci_machine_fields, parse_nvidia_line, rate,
        refine_linux_gpu_name,
    };
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
    fn lspci_parser_extracts_human_readable_device_name() {
        let fields = parse_lspci_machine_fields(
            r#"03:00.0 "VGA compatible controller" "Advanced Micro Devices, Inc. [AMD/ATI]" "Navi 31 [Radeon RX 7900 XT/7900 XTX/7900 GRE/7900M]" -rcc -p00 "XFX Limited" "Device 7905""#,
        )
        .expect("parse lspci output");
        assert_eq!(
            fields.get(2).map(String::as_str),
            Some("Navi 31 [Radeon RX 7900 XT/7900 XTX/7900 GRE/7900M]")
        );
    }

    #[test]
    fn lspci_parser_unescapes_quoted_fields() {
        let fields = parse_lspci_machine_fields(
            r#"01:00.0 "Display controller" "Vendor" "Model \"Edition\"" -r01"#,
        )
        .expect("parse lspci output");
        assert_eq!(fields.get(2).map(String::as_str), Some("Model \"Edition\""));
    }

    #[test]
    fn grouped_navi_31_name_uses_vram_capacity_when_unambiguous() {
        assert_eq!(
            refine_linux_gpu_name(
                "Navi 31 [Radeon RX 7900 XT/7900 XTX/7900 GRE/7900M]",
                Some(21_458_059_264),
            ),
            "Radeon RX 7900 XT"
        );
        assert_eq!(
            refine_linux_gpu_name(
                "Navi 31 [Radeon RX 7900 XT/7900 XTX/7900 GRE/7900M]",
                Some(24 * 1024 * 1024 * 1024),
            ),
            "Radeon RX 7900 XTX"
        );
        assert_eq!(
            refine_linux_gpu_name(
                "Navi 31 [Radeon RX 7900 XT/7900 XTX/7900 GRE/7900M]",
                Some(16 * 1024 * 1024 * 1024),
            ),
            "Navi 31 [Radeon RX 7900 XT/7900 XTX/7900 GRE/7900M]"
        );
    }

    #[test]
    fn network_totals_are_empty_for_new_collection() {
        let networks = Networks::new();
        assert_eq!(network_totals(&networks), (0, 0));
    }

    #[test]
    fn network_only_collection_does_not_publish_system_metrics() {
        let mut collector = super::Collector::new("android", true);
        assert!(collector.desktop.is_none());
        let metrics = collector.sample();
        assert_eq!(metrics.cpu_model, None);
        assert_eq!(metrics.memory_available_bytes, None);
        assert_eq!(metrics.swap_total_bytes, None);
        assert_eq!(metrics.load_average, None);
        assert_eq!(metrics.uptime_seconds, None);
        assert!(metrics.disks.is_empty());
        assert!(metrics.gpus.is_empty());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_gpu_probe_line_keeps_unavailable_fields_empty() {
        assert_eq!(
            super::parse_windows_gpu_line("12;1048576"),
            Some((Some(12.0), Some(1_048_576)))
        );
        assert_eq!(super::parse_windows_gpu_line(";0"), Some((None, Some(0))));
        assert_eq!(super::parse_windows_gpu_line(";"), None);
        assert_eq!(super::parse_windows_gpu_line("12;1048576;0"), None);
        assert_eq!(super::parse_windows_gpu_line("unexpected output"), None);
    }
}
