// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The kernel args in this file are used to force developers to list all their assembly-supplied
//! kernel arguments in this central location. The resulting enums are iterated over in order to
//! generate scrutiny golden files.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use strum_macros::EnumIter;

/// Kernel arguments that can be used by assembly subsystems.
#[derive(Debug, Clone, EnumIter, Serialize)]
#[serde(into = "String")]
pub enum KernelArg {
    /// When searching for a CPU on which to place a task, prefer little cores over big cores.
    /// Enabling this option trades off improved performance in favor of reduced power consumption.
    ///
    /// DEPRECATED - This option is scheduled to be removed in an upcoming release.  Do not take any
    /// critical dependencies on it.
    SchedulerPreferLittleCpus(bool),

    /// This controls the degree of logging to the serial console in the kernel's early
    /// boot phase; if false, only error-related logging will take place.
    ///
    /// One utility of this option is for benchmarking: synchronous, single-threaded
    /// UART writing can be relatively costly (10 chars/ms) to the entire time
    /// spent in physboot and it is desirable to exclude this sort of work from
    /// holistic time measurements.
    PhysVerbose(bool),

    /// This controls what serial port is used.  If provided, it overrides the serial
    /// port described by the system's bootdata.  The kernel debug serial port is
    /// a reserved resource and may not be used outside of the kernel.
    Serial(String),

    /// This option triggers eviction of file pages at the Warning pressure state,
    /// in addition to the default behavior, which is to evict at the Critical and OOM
    /// states.
    OomEvictAtWarning(bool),

    /// This option configures kernel eviction to run continually in the background to try and
    /// keep the system out of memory pressure, as opposed to triggering one-shot eviction only at
    /// memory pressure level transitions.
    OomEvictContinuous(bool),

    /// This option specifies the free-memory threshold at which the out-of-memory (OOM)
    /// thread will trigger an out-of-memory event and begin killing processes, or
    /// rebooting the system.
    OomOutOfMemoryMib(u32),

    /// This option specifies the free-memory threshold at which the out-of-memory
    /// (OOM) thread will trigger a critical memory pressure event, signaling that
    /// processes should free up memory.
    OomCriticalMib(u32),

    /// This option specifies the free-memory threshold at which the out-of-memory
    /// (OOM) thread will trigger a warning memory pressure event, signaling that
    /// processes should slow down memory allocations.
    OomWarningMib(u32),

    /// If this option is set, the system will halt on a kernel panic instead
    /// of rebooting.
    HaltOnPanic(bool),

    /// For address spaces that use ASLR this controls the number of bits of entropy in
    /// the randomization. Higher entropy results in a sparser address space and uses
    /// more memory for page tables. Valid values range from 0-36.
    AslrEntropyBits(u8),

    /// When enabled and if jitterentropy fails at initial seeding, CPRNG panics.
    CprngSeedRequireJitterEntropy(bool),

    /// When enabled and if you do not provide entropy input from the kernel
    /// command line, CPRNG panics.
    CprngSeedRequireCmdline(bool),

    /// When enabled and if jitterentropy fails at reseeding, CPRNG panics.
    CprngReseedRequireJitterEntropy(bool),

    /// This option sets an upper-bound in megabytes for the system memory.
    /// If set to zero, then no upper-bound is set.
    ///
    /// For example, choosing a low enough value would allow a user simulating a system with
    /// less physical memory than it actually has.
    MemoryLimitMib(u64),

    /// If set, tries to initialize the dap debug aperture at a hard coded address for the particular
    /// system on chip.
    Arm64DebugDap(Arm64DebugDapSoc),

    /// This option causes the kernels active memory scanner to be initially
    /// enabled on startup. You can also enable and disable it using the kernel
    /// console. If you disable the scanner, you can have additional system
    /// predictability since it removes time based and background memory eviction.
    ///
    /// Every action the scanner performs can be individually configured and disabled.
    /// If all actions are disabled then enabling the scanner has no effect.
    PageScannerStartAtBoot(bool),

    /// When set, allows the page scanner to evict user pager backed pages. Eviction can
    /// reduce memory usage and prevent out of memory scenarios, but removes some
    /// timing predictability from system behavior.
    PageScannerEnableEviction(bool),

    /// This option configures the maximal number of candidate pages the zero
    /// page scanner will consider every second.
    ///
    /// The page scanner must be running for this option to have any effect. It
    /// can be enabled at boot unless `disable_at_boot` is set to True.
    PageScannerZeroPageScanCount(ZeroPageScanCount),
}

/// Options for zero page scanner configuration.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, JsonSchema, EnumIter)]
#[serde(rename_all = "snake_case")]
pub enum ZeroPageScanCount {
    /// Default value is 20000. This value was chosen to consume, in the worst
    /// case, 5% CPU on a lower-end arm device. Individual configurations may
    /// wish to tune this higher (or lower) as needed.
    #[default]
    Default,

    /// No zero page scanning will occur. This can
    /// provide additional system predictability for benchmarking or other
    /// workloads.
    NoScans,

    /// The number of scans that should occur per second.
    PerSecond(u64),
}

impl std::fmt::Display for ZeroPageScanCount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Default => "20000".to_string(),
                Self::NoScans => "0".to_string(),
                Self::PerSecond(i) => format!("{}", i),
            }
        )
    }
}

/// This enum lists all the supported soc that can enable DAP
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize, EnumIter)]
pub enum Arm64DebugDapSoc {
    /// Enable DAP for amlogic-t931g
    #[serde(rename = "amlogic-t931g")]
    #[default]
    AmlogicT931g,

    /// Enable DAP for amlogic-s905d2
    #[serde(rename = "amlogic-s905d2")]
    AmlogicS905d2,

    /// Enable DAP for amlogic-s905d3g
    #[serde(rename = "amlogic-s905d3g")]
    AmlogicS905d3g,

    /// Enable DAP for amlogic-a311d
    #[serde(rename = "amlogic-a311d")]
    AmlogicA311d,
}

impl std::fmt::Display for Arm64DebugDapSoc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AmlogicT931g => write!(f, "amlogic-t931g"),
            Self::AmlogicS905d2 => write!(f, "amlogic-s905d2"),
            Self::AmlogicS905d3g => write!(f, "amlogic-s905d3g"),
            Self::AmlogicA311d => write!(f, "amlogic-a311d"),
        }
    }
}

impl std::fmt::Display for KernelArg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (key, value) = self.key_and_value();
        write!(f, "{}={}", key, value)
    }
}

impl KernelArg {
    /// Retrieve the key half of the kernel cmdline argument.
    fn key_and_value(&self) -> (String, String) {
        let (key, value) = match self {
            Self::SchedulerPreferLittleCpus(b) => {
                ("kernel.scheduler.prefer-little-cpus", b.to_string())
            }
            Self::PhysVerbose(b) => ("kernel.phys.verbose", b.to_string()),
            Self::Serial(s) => ("kernel.serial", s.to_string()),
            Self::OomEvictAtWarning(b) => ("kernel.oom.evict-at-warning", b.to_string()),
            Self::OomEvictContinuous(b) => ("kernel.oom.evict-continuous", b.to_string()),
            Self::OomOutOfMemoryMib(i) => ("kernel.oom.outofmemory-mb", i.to_string()),
            Self::OomCriticalMib(i) => ("kernel.oom.critical-mb", i.to_string()),
            Self::OomWarningMib(i) => ("kernel.oom.warning-mb", i.to_string()),
            Self::HaltOnPanic(b) => ("kernel.halt-on-panic", b.to_string()),
            Self::AslrEntropyBits(i) => ("aslr.entropy_bits", i.to_string()),
            Self::CprngSeedRequireJitterEntropy(b) => {
                ("kernel.cprng-seed-require.jitterentropy", b.to_string())
            }
            Self::CprngSeedRequireCmdline(b) => {
                ("kernel.cprng-seed-require.cmdline", b.to_string())
            }
            Self::CprngReseedRequireJitterEntropy(b) => {
                ("kernel.cprng-reseed-require.jitterentropy", b.to_string())
            }
            Self::MemoryLimitMib(i) => ("kernel.memory-limit-mb", i.to_string()),
            Self::Arm64DebugDap(s) => ("kernel.arm64.debug.dap-rom-soc", s.to_string()),
            Self::PageScannerStartAtBoot(b) => ("kernel.page-scanner.start-at-boot", b.to_string()),
            Self::PageScannerEnableEviction(b) => {
                ("kernel.page-scanner.enable-eviction", b.to_string())
            }
            Self::PageScannerZeroPageScanCount(z) => {
                ("kernel.page-scanner.zero-page-scans-per-second", z.to_string())
            }
        };
        (key.to_string(), value)
    }

    /// Lines to place in the scrutiny allowlist for this particular kernel argument.
    pub fn allowlist_entries(&self) -> Vec<String> {
        let key = self.key_and_value().0;
        match self {
            // These kernel args do not have any product-provided pieces, therefore they can be
            // serialized as-is.
            Self::SchedulerPreferLittleCpus(_)
            | Self::OomEvictAtWarning(_)
            | Self::OomEvictContinuous(_)
            | Self::HaltOnPanic(_)
            | Self::PageScannerStartAtBoot(_)
            | Self::PhysVerbose(_)
            | Self::PageScannerEnableEviction(_)
            | Self::CprngReseedRequireJitterEntropy(_)
            | Self::CprngSeedRequireCmdline(_)
            | Self::CprngSeedRequireJitterEntropy(_) => {
                vec![format!("{}=true", key), format!("{}=false", key)]
            }

            // These kernel args have their right half provided by the product, therefore we
            // serialize them with a =* to catch all options.
            Self::Serial(_)
            | Self::OomOutOfMemoryMib(_)
            | Self::OomCriticalMib(_)
            | Self::OomWarningMib(_)
            | Self::AslrEntropyBits(_)
            | Self::MemoryLimitMib(_)
            | Self::Arm64DebugDap(_)
            | Self::PageScannerZeroPageScanCount(_) => {
                vec![format!("{}=*", key)]
            }
        }
    }
}

impl From<KernelArg> for String {
    fn from(b: KernelArg) -> Self {
        b.to_string()
    }
}
