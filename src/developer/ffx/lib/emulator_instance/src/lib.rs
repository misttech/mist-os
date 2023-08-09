// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use nix;
pub use sdk_metadata::{AudioDevice, DataAmount, DataUnits, PointingDevice, Screen};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    ops::Deref,
    path::{Path, PathBuf},
    time::Duration,
};

mod enumerations;
mod instances;

pub use enumerations::{
    AccelerationMode, ConsoleType, CpuArchitecture, EngineState, EngineType, GpuType, LogLevel,
    NetworkingMode, OperatingSystem, VirtualCpu,
};

pub use instances::{
    clean_up_instance_dir, get_all_instances, get_instance_dir, read_from_disk,
    read_from_disk_untyped, write_to_disk, EMU_INSTANCE_ROOT_DIR,
};

/// Holds a single mapping from a host port to the guest.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct PortMapping {
    pub guest: u16,
    pub host: Option<u16>,
}

/// Used when reading the instance data as a return value.
#[derive(Debug)]
pub enum EngineOption {
    DoesExist(EmulatorInstanceData),
    DoesNotExist(String),
}

pub trait EmulatorInstanceInfo {
    fn get_name(&self) -> &str;
    fn is_running(&self) -> bool;
    fn get_engine_state(&self) -> EngineState;
    fn get_engine_type(&self) -> EngineType;
    fn get_pid(&self) -> u32;
    fn get_emulator_configuration(&self) -> &EmulatorConfiguration;
    fn get_emulator_configuration_mut(&mut self) -> &mut EmulatorConfiguration;
    fn get_emulator_binary(&self) -> &PathBuf;
    fn get_networking_mode(&self) -> &NetworkingMode;
    fn get_ssh_port(&self) -> Option<u16>;
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct FlagData {
    /// Arguments. The set of flags which follow the "-fuchsia" option. These are not processed by
    /// Femu, but are passed through to Qemu.
    pub args: Vec<String>,

    /// Environment Variables. These are not passed on the command line, but are set in the
    /// process's environment before execution.
    pub envs: HashMap<String, String>,

    /// Features. A Femu-only field. Features are the first set of command line flags passed to the
    /// Femu binary. These are single words, capitalized, comma-separated, and immediately follow
    /// the flag "-feature".
    pub features: Vec<String>,

    /// Kernel Arguments. The last part of the command line. A set of text values that are passed
    /// through the emulator executable directly to the guest system's kernel.
    pub kernel_args: Vec<String>,

    /// Options. A Femu-only field. Options come immediately after features. Options may be boolean
    /// flags (e.g. -no-hidpi-scaling) or have associated values (e.g. -window-size 1280x800).
    pub options: Vec<String>,
}

/// A pre-formatted disk image containing the base packages of the system.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum DiskImage {
    Fvm(PathBuf),
    Fxfs(PathBuf),
}

impl AsRef<Path> for DiskImage {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl Deref for DiskImage {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        match self {
            DiskImage::Fvm(path) => path,
            DiskImage::Fxfs(path) => path,
        }
    }
}

/// Image files and other information specific to the guest OS.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct GuestConfig {
    /// The guest's virtual storage device.
    pub disk_image: Option<DiskImage>,

    /// The Fuchsia kernel, which loads alongside the ZBI and brings up the OS.
    pub kernel_image: PathBuf,

    /// Zircon Boot image, this is Fuchsia's initial ram disk used in the boot process.
    pub zbi_image: PathBuf,

    /// Hash of zbi_image. Used to detect changes when reusing an emulator instance.
    #[serde(default)]
    pub zbi_hash: String,

    /// Hash of disk_image. Used to detect changes when reusing an emulator instance.
    #[serde(default)]
    pub disk_hash: String,
}

/// Host-side configuration data, such as physical hardware and host OS details.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct HostConfig {
    /// Determines the type of hardware acceleration to use for emulation, such as KVM.
    pub acceleration: AccelerationMode,

    /// Indicates the CPU architecture of the host system.
    pub architecture: CpuArchitecture,

    /// Determines the type of graphics acceleration, to improve rendering in the guest OS.
    pub gpu: GpuType,

    /// Specifies the path to the emulator's log files.
    pub log: PathBuf,

    /// Determines the networking type for the emulator.
    pub networking: NetworkingMode,

    /// Indicates the operating system the host system is running.
    pub os: OperatingSystem,

    /// Holds a set of named ports, with the mapping from host to guest for each one.
    /// Generally only useful when networking is set to "user".
    pub port_map: HashMap<String, PortMapping>,
}

/// A collection of properties which control/influence the
/// execution of an emulator instance. These are different from the
/// DeviceConfig and GuestConfig which defines the hardware configuration
/// and behavior of Fuchsia running within the emulator instance.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct RuntimeConfig {
    /// Additional arguments to pass directly to the emulator.
    #[serde(default)]
    pub addl_kernel_args: Vec<String>,

    /// A flag to indicate that the --config flag was used to override the standard configuration.
    /// This matters because the contents of the EmulatorConfiguration no longer represent a
    /// consistent description of the emulator instance.
    #[serde(default)]
    pub config_override: bool,

    /// The emulator's output, which might come from the serial console, the guest, or nothing.
    pub console: ConsoleType,

    /// Pause the emulator and wait for the user to attach a debugger to the process.
    pub debugger: bool,

    /// Engine type name. Added here to be accessible in the configuration template processing.
    #[serde(default)]
    pub engine_type: EngineType,

    /// Run the emulator without a GUI. Graphics drivers will still be loaded.
    pub headless: bool,

    /// On machines with high-density screens (such as MacBook Pro), window size may be
    /// scaled to match the host's resolution which results in a much smaller GUI.
    pub hidpi_scaling: bool,

    /// The staging and working directory for the emulator instance.
    pub instance_directory: PathBuf,

    /// The verbosity level of the logs for this instance.
    pub log_level: LogLevel,

    // A generated MAC address for the emulators virtual network.
    pub mac_address: String,

    /// The human-readable name for this instance. Must be unique from any other current
    /// instance on the host.
    pub name: String,

    /// Whether or not the emulator should reuse a previous instance's image files.
    #[serde(default)]
    pub reuse: bool,

    /// Maximum amount of time to wait on the emulator health check to succeed before returning
    /// control to the user.
    pub startup_timeout: Duration,

    /// Path to an enumeration flags template file, which contains a Handlebars-renderable
    /// set of arguments to be passed to the Command which starts the emulator.
    pub template: PathBuf,

    /// Optional path to a Tap upscript file, which is passed to the emulator when Tap networking
    /// is enabled.
    pub upscript: Option<PathBuf>,
}

/// Specifications of the virtual device to be emulated.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct DeviceConfig {
    /// The model of audio device being emulated, if any.
    pub audio: AudioDevice,

    /// The architecture and number of CPUs to emulate on the guest system.
    pub cpu: VirtualCpu,

    /// The amount of virtual memory to emulate on the guest system.
    pub memory: DataAmount,

    /// Which input source to emulate for screen interactions on the guest, if any.
    pub pointing_device: PointingDevice,

    /// The dimensions of the virtual device's screen, if any.
    pub screen: Screen,

    /// The amount of virtual storage to allocate to the guest's storage device, which will be
    /// populated by the GuestConfig's fvm_image. Only one virtual storage device is supported
    /// at this time.
    pub storage: DataAmount,
}

/// Collects the specific configurations into a single struct for ease of passing around.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct EmulatorConfiguration {
    pub device: DeviceConfig,
    pub flags: FlagData,
    pub guest: GuestConfig,
    pub host: HostConfig,
    pub runtime: RuntimeConfig,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct EmulatorInstanceData {
    #[serde(default)]
    pub(crate) emulator_binary: PathBuf,
    pub(crate) emulator_configuration: EmulatorConfiguration,
    pub(crate) pid: u32,
    pub(crate) engine_type: EngineType,
    #[serde(default)]
    pub(crate) engine_state: EngineState,
}

impl EmulatorInstanceData {
    pub fn new_with_state(name: &str, state: EngineState) -> Self {
        let mut ret = Self::default();
        ret.emulator_configuration.runtime.name = name.to_string();
        ret.engine_state = state;
        ret
    }
    pub fn new(
        emulator_configuration: EmulatorConfiguration,
        engine_type: EngineType,
        engine_state: EngineState,
    ) -> Self {
        EmulatorInstanceData {
            emulator_configuration,
            engine_state,
            engine_type,
            ..Default::default()
        }
    }

    pub fn set_engine_state(&mut self, state: EngineState) {
        self.engine_state = state
    }
    pub fn set_pid(&mut self, pid: u32) {
        self.pid = pid
    }

    pub fn set_emulator_binary(&mut self, binary: PathBuf) {
        self.emulator_binary = binary
    }

    pub fn set_engine_type(&mut self, engine_type: EngineType) {
        self.engine_type = engine_type
    }

    pub fn set_instance_directory(&mut self, instance_dir: &str) {
        self.emulator_configuration.runtime.instance_directory = instance_dir.into()
    }
}

impl EmulatorInstanceInfo for EmulatorInstanceData {
    fn get_name(&self) -> &str {
        &self.emulator_configuration.runtime.name
    }

    fn is_running(&self) -> bool {
        is_pid_running(self.pid)
    }
    fn get_engine_state(&self) -> EngineState {
        // If the static state is running, compare it with the process state
        // They can get out of sync, for example when rebooting the host.
        match self.engine_state {
            EngineState::Running if self.is_running() => EngineState::Running,
            EngineState::Running if !self.is_running() => EngineState::Staged,
            _ => self.engine_state,
        }
    }
    fn get_engine_type(&self) -> EngineType {
        self.engine_type
    }
    fn get_pid(&self) -> u32 {
        self.pid
    }
    fn get_emulator_configuration(&self) -> &EmulatorConfiguration {
        &self.emulator_configuration
    }
    fn get_emulator_configuration_mut(&mut self) -> &mut EmulatorConfiguration {
        &mut self.emulator_configuration
    }
    fn get_emulator_binary(&self) -> &PathBuf {
        &self.emulator_binary
    }
    fn get_networking_mode(&self) -> &NetworkingMode {
        &self.emulator_configuration.host.networking
    }
    fn get_ssh_port(&self) -> Option<u16> {
        if let Some(ssh) = self.emulator_configuration.host.port_map.get("ssh") {
            return ssh.host;
        }
        None
    }
}

/// Returns true if the process identified by the pid is running.
fn is_pid_running(pid: u32) -> bool {
    if pid != 0 {
        // First do a no-hang wait to collect the process if it's defunct.
        let _ = nix::sys::wait::waitpid(
            nix::unistd::Pid::from_raw(pid.try_into().unwrap()),
            Some(nix::sys::wait::WaitPidFlag::WNOHANG),
        );
        // Check to see if it is running by sending signal 0. If there is no error,
        // the process is running.
        return nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid.try_into().unwrap()), None)
            .is_ok();
    }
    return false;
}
