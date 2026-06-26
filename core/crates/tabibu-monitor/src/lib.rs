//! Per-process and system resource sampling for the monitor agent and the
//! Health views. Built on `sysinfo`; numbers are reported exactly as the OS
//! provides them. Memory *pressure* is NOT computed here — the shell reads
//! the real signal via `DispatchSource.makeMemoryPressureSource` and
//! `NSProcessInfo.thermalState`; this crate only supplies raw usage figures.
//!
//! Budget note: the sampler itself is on the monitor's resource budget
//! (<1% CPU, <30 MB RSS). Keep allocations flat:
//! one `System`, refreshed in place.

use serde::Serialize;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

mod rosetta;

#[derive(Debug, Clone, Serialize)]
pub struct ProcessSample {
    pub pid: u32,
    pub name: String,
    /// CPU usage in percent of one core (can exceed 100 for multi-threaded).
    pub cpu_percent: f32,
    /// Resident memory in bytes.
    pub memory_bytes: u64,
    pub exe_path: Option<String>,
    /// Whether the process is running translated under Rosetta 2.
    /// `None` = unknown (sysctl failed or process gone),
    /// `Some(true)` = Rosetta (x86_64 on Apple Silicon),
    /// `Some(false)` = native. Populated only for the returned top-N
    /// processes; see [`Sampler::sample`].
    pub is_translated: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SystemSample {
    pub total_memory_bytes: u64,
    pub used_memory_bytes: u64,
    pub total_swap_bytes: u64,
    pub used_swap_bytes: u64,
    /// Average CPU usage across all cores, percent.
    pub cpu_percent: f32,
    /// Top processes by the requested ordering.
    pub top_processes: Vec<ProcessSample>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TopBy {
    Cpu,
    Memory,
}

/// Stateful sampler. CPU percentages need two refreshes spaced apart; call
/// [`Sampler::sample`] on your interval and the deltas come out right.
pub struct Sampler {
    sys: System,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    #[must_use]
    pub fn new() -> Self {
        let sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_memory(sysinfo::MemoryRefreshKind::everything())
                .with_cpu(sysinfo::CpuRefreshKind::everything())
                .with_processes(
                    ProcessRefreshKind::nothing()
                        .with_cpu()
                        .with_memory()
                        .with_exe(sysinfo::UpdateKind::OnlyIfNotSet),
                ),
        );
        Self { sys }
    }

    /// Refresh and return a snapshot with the top `n` processes by `by`.
    pub fn sample(&mut self, n: usize, by: TopBy) -> SystemSample {
        self.sys.refresh_memory();
        self.sys.refresh_cpu_usage();
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_exe(sysinfo::UpdateKind::OnlyIfNotSet),
        );

        let mut procs: Vec<ProcessSample> = self
            .sys
            .processes()
            .iter()
            .map(|(pid, p)| ProcessSample {
                pid: pid.as_u32(),
                name: p.name().to_string_lossy().into_owned(),
                cpu_percent: p.cpu_usage(),
                memory_bytes: p.memory(),
                exe_path: p.exe().map(|e| e.to_string_lossy().into_owned()),
                // Filled in below for the top-N only; default unknown.
                is_translated: None,
            })
            .collect();
        match by {
            TopBy::Cpu => procs.sort_by(|a, b| {
                b.cpu_percent
                    .partial_cmp(&a.cpu_percent)
                    .unwrap_or(std::cmp::Ordering::Equal)
            }),
            TopBy::Memory => procs.sort_by_key(|p| std::cmp::Reverse(p.memory_bytes)),
        }
        procs.truncate(n);

        // Rosetta detection issues one `sysctl` per pid, so we run it only
        // after `truncate(n)` — i.e. for the processes we actually return —
        // to keep the sampler within the monitor CPU budget rather than
        // probing every process on the system.
        for p in &mut procs {
            p.is_translated = rosetta::process_is_translated(p.pid);
        }

        SystemSample {
            total_memory_bytes: self.sys.total_memory(),
            used_memory_bytes: self.sys.used_memory(),
            total_swap_bytes: self.sys.total_swap(),
            used_swap_bytes: self.sys.used_swap(),
            cpu_percent: self.sys.global_cpu_usage(),
            top_processes: procs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_returns_real_data() {
        let mut s = Sampler::new();
        // First sample primes CPU counters; second yields meaningful deltas.
        let _ = s.sample(5, TopBy::Cpu);
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let snap = s.sample(5, TopBy::Memory);

        assert!(snap.total_memory_bytes > 0);
        assert!(snap.used_memory_bytes > 0);
        assert!(snap.used_memory_bytes <= snap.total_memory_bytes);
        assert!(!snap.top_processes.is_empty());
        // Memory ordering holds.
        let mems: Vec<u64> = snap.top_processes.iter().map(|p| p.memory_bytes).collect();
        assert!(mems.windows(2).all(|w| w[0] >= w[1]));
        // Each returned process exposes a translation verdict without panic
        // (value may be None for a process that exited between sampling steps).
        for p in &snap.top_processes {
            let _ = p.is_translated;
        }
        // Serializes for the FFI boundary.
        assert!(serde_json::to_string(&snap).is_ok());
    }
}
