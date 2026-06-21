use sysinfo::System;

use super::SystemInfo;

pub fn collect() -> SystemInfo {
    let mut sys = System::new_all();
    sys.refresh_all();

    let hostname = System::host_name().unwrap_or_else(|| "?".into());
    let os_version = System::long_os_version().unwrap_or_else(|| "?".into());
    let uptime_secs = System::uptime();

    // Refresh CPU usage twice with a short delay for meaningful measurement
    sys.refresh_cpu_usage();
    std::thread::sleep(std::time::Duration::from_millis(200));
    sys.refresh_cpu_usage();
    let cpu_percent = sys.global_cpu_usage();

    let mem_total = sys.total_memory();
    let mem_used = sys.used_memory();

    SystemInfo {
        hostname,
        os_version,
        uptime_secs,
        cpu_percent,
        mem_used,
        mem_total,
    }
}
