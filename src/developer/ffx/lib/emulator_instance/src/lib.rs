// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Context as _, Result};
use schemars::JsonSchema;
pub use sdk_metadata::{AudioDevice, DataAmount, DataUnits, PointingDevice, Screen, VsockDevice};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod enumerations;
pub mod fletcher64;
mod instances;
pub mod targets;

pub use enumerations::{
    AccelerationMode, ConsoleType, CpuArchitecture, EngineState, EngineType, GpuType, LogLevel,
    NetworkingMode, OperatingSystem, VirtualCpu,
};
use fletcher64::get_file_hash;
pub use instances::{
    read_from_disk, read_from_disk_untyped, write_to_disk, EmulatorInstances, EMU_INSTANCE_ROOT_DIR,
};
pub use targets::{
    get_all_targets, start_emulator_watching, EmulatorTargetAction, EmulatorWatcher,
};

/// Holds a single mapping from a host port to the guest.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
pub struct PortMapping {
    pub guest: u16,
    pub host: Option<u16>,
}

/// Used when reading the instance data as a return value.
// TODO(https://fxbug.dev/324167674): fix.
#[allow(clippy::large_enum_variant)]
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

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize, JsonSchema)]
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

/// A pre-formatted disk image to be used by the emulator
///
/// The disk images may contain the base packages of the system, or multiple data partitions.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum DiskImage {
    Fat(PathBuf),
    Fvm(PathBuf),
    Fxfs(PathBuf),
    Gpt(PathBuf),
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
            DiskImage::Fat(path) => path,
            DiskImage::Fvm(path) => path,
            DiskImage::Fxfs(path) => path,
            DiskImage::Gpt(path) => path,
        }
    }
}

/// Image files and other information specific to the guest OS.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct GuestConfig {
    /// The guest's virtual storage device.
    pub disk_image: Option<DiskImage>,

    /// The Fuchsia kernel, which loads alongside the ZBI and brings up the OS.
    /// This can also be a efi image, in which case there is no zbi needed.
    /// There can also be bootloader images (.fatfs) that boot directly and do not
    /// require a kernel.
    pub kernel_image: Option<PathBuf>,

    /// Zircon Boot image, this is Fuchsia's initial ram disk used in the boot process.
    /// Note: This is not used if the kernel image is an efi image.
    pub zbi_image: Option<PathBuf>,

    /// Hash of zbi_image or kernel if the kernel is efi. Used to detect changes when reusing
    /// an emulator instance.
    #[serde(default)]
    pub zbi_hash: String,

    /// Path to a PEM style key file. This is used when re-signing a vbmeta file for a modified ZBI.
    #[serde(default)]
    pub vbmeta_key_file: Option<PathBuf>,

    // Path to the key metadata. This is required when a PEM file is used to re-sign a vbmeta file.
    #[serde(default)]
    pub vbmeta_key_metadata_file: Option<PathBuf>,

    /// Hash of disk_image. Used to detect changes when reusing an emulator instance.
    #[serde(default)]
    pub disk_hash: String,

    /// Firmware emulation files, primarily used only if the guest is efi. The code is usually
    /// read-only.
    #[serde(default)]
    pub ovmf_code: PathBuf,

    /// Firmware emulation data file. This is read-write, so it should be unique per emulator instance.
    #[serde(default)]
    pub ovmf_vars: PathBuf,

    /// Path to the product bundle from where the emulator is staged.
    /// TODO(https://fxbug.dev/381263769): Note that this is passed to make-fuchsia-vol for
    /// constructing GPT images until a better solution is in place. Please avoid using it, as it
    /// may go away without warning.
    #[serde(default)]
    pub product_bundle_path: Option<PathBuf>,

    /// Whether this guest is running from a GPT partitioned full disk image.
    /// Note that this is used to pass the info whether this is a GPT-image based instance to the
    /// arguments template parsing in arg_templates.rs as it cannot call `is_gpt()`.
    #[serde(default)]
    pub is_gpt: bool,
}

impl GuestConfig {
    pub fn is_efi(&self) -> bool {
        match &self.kernel_image {
            Some(file_path) => file_path.extension().unwrap_or_default() == "efi",
            None => {
                if let Some(DiskImage::Fat(_)) = self.disk_image {
                    true
                } else {
                    false
                }
            }
        }
    }

    pub fn is_gpt(&self) -> bool {
        if let Some(DiskImage::Gpt(_)) = self.disk_image {
            true
        } else {
            false
        }
    }

    pub fn get_image_hashes(&self) -> Result<(u64, u64)> {
        // If there is an efi kernel, and no zbi, use the kernel to calculate the hash.

        let zbi_hash = match &self.zbi_image {
            Some(zbi_path) => get_file_hash(zbi_path).context("could not calculate zbi hash")?,
            None => {
                if let Some(file_path) = &self.kernel_image {
                    get_file_hash(file_path).context("could not calculate efi hash")?
                } else {
                    0
                }
            }
        };

        let disk_hash = if let Some(disk) = &self.disk_image {
            get_file_hash(disk.as_ref()).context("could not calculate disk hash")?
        } else {
            0
        };

        Ok((zbi_hash, disk_hash))
    }

    pub fn save_disk_hashes(&mut self) -> Result<()> {
        let (new_zbi_hash, new_disk_hash) = self.get_image_hashes()?;
        self.zbi_hash = format!("{new_zbi_hash:x}");
        self.disk_hash = format!("{new_disk_hash:x}");
        Ok(())
    }

    pub fn check_required_files(&self) -> Result<()> {
        let kernel_path: &_ = &self.kernel_image;
        let zbi_path = &self.zbi_image;
        let disk_image_path = &self.disk_image;

        // If no kernel is provided, a FAT diskimage or a full GPT disk containing the bootloader
        // needs to be present.
        match kernel_path {
            Some(file_path) => {
                if !file_path.exists() {
                    bail!("kernel file {:?} does not exist.", kernel_path);
                }
            }
            None => match disk_image_path {
                Some(DiskImage::Fat(fat_path)) => {
                    if !fat_path.exists() {
                        bail!("FAT file {:?} does not exist.", fat_path);
                    }
                }
                Some(DiskImage::Gpt(gpt_path)) => {
                    if !gpt_path.exists() {
                        bail!("Full GPT disk file {:?} does not exist.", gpt_path);
                    }
                }
                _ => {
                    bail!("No kernel file or bootloader file configured.");
                }
            },
        };

        if let Some(file_path) = zbi_path.as_ref() {
            if !file_path.exists() {
                bail!("zbi file {:?} does not exist.", zbi_path);
            }
        }

        if let Some(file_path) = disk_image_path.as_ref() {
            if !file_path.exists() {
                bail!("disk image file {:?} does not exist.", file_path);
            }
        }
        Ok(())
    }
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
    /// Additional arguments to pass directly to the guest kernel.
    #[serde(default)]
    pub addl_kernel_args: Vec<String>,

    /// Additional arguments to pass directly to the emulator.
    #[serde(default)]
    pub addl_emu_args: Vec<String>,

    /// Additional environment variables to use when starting the emulator
    #[serde(default)]
    pub addl_env: HashMap<String, String>,

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
    /// If this is None, the internal emulator_flags.json.template is used.
    pub template: Option<PathBuf>,

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

    /// The amount of virtual storage to allocate to the guest's storage device, which will be
    /// populated by the GuestConfig's fvm_image. Only one virtual storage device is supported
    /// at this time.
    pub vsock: Option<VsockDevice>,
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
