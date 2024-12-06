// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

use std::collections::BTreeSet;

use crate::common::{PackageDetails, PackagedDriverDetails};
use crate::platform_config::sysmem_config::BoardSysmemConfig;
use assembly_constants::Arm64DebugDapSoc;
use assembly_file_relative_path::{FileRelativePathBuf, SupportsFileRelativePaths};
use assembly_images_config::BoardFilesystemConfig;
use serde::{Deserialize, Serialize};

/// This struct provides information about the "board" that a product is being
/// assembled to run on.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, SupportsFileRelativePaths)]
#[serde(deny_unknown_fields)]
pub struct BoardInformation {
    /// The name of the board.
    pub name: String,

    /// Metadata about the board that's provided to the 'fuchsia.hwinfo.Board'
    /// protocol and to the Board Driver via the PlatformID and BoardInfo ZBI
    /// items.
    #[serde(default)]
    pub hardware_info: HardwareInfo,

    /// The "features" that this board provides to the product.
    ///
    /// NOTE: This is a still-evolving, loosely-coupled, set of identifiers.
    /// It's an unstable interface between the boards and the platform.
    #[serde(default)]
    pub provided_features: Vec<String>,

    /// Path to the devicetree binary (.dtb) this provided by this board.
    #[serde(default)]
    #[file_relative_paths]
    pub devicetree: Option<FileRelativePathBuf>,

    /// Configuration for the various filesystems that the product can choose to
    /// include.
    #[serde(default)]
    #[file_relative_paths]
    pub filesystems: BoardFilesystemConfig,

    /// These are paths to the directories that are board input bundles that
    /// this board configuration includes.  Product assembly will always include
    /// these into the images that it creates.
    ///
    /// These are the board-specific artifacts that the Fuchsia platform needs
    /// added to the assembled system in order to be able to boot Fuchsia on
    /// this board.
    ///
    /// Examples:
    ///  - the "board driver"
    ///  - storage drivers
    ///
    /// If any of these artifacts are removed, even the 'bootstrap' feature set
    /// may be unable to boot.
    #[serde(default)]
    #[file_relative_paths]
    pub input_bundles: Vec<FileRelativePathBuf>,

    /// Consolidated configuration from all of the BoardInputBundles.  This is
    /// not deserialized from the BoardConfiguration, but is instead created by
    /// parsing each of the input_bundles and merging their configuration fields.
    #[serde(skip)]
    #[file_relative_paths]
    pub configuration: BoardProvidedConfig,

    /// Configure kernel cmdline args
    /// TODO: Move this into platform section below
    #[serde(default)]
    pub kernel: BoardKernelConfig,

    /// Configure platform related feature
    #[serde(default)]
    pub platform: PlatformConfig,

    /// GUIDs for the TAs provided by this board's TEE driver.
    #[serde(default)]
    pub tee_trusted_app_guids: Vec<uuid::Uuid>,
}

/// This struct defines board-provided data for the 'fuchsia.hwinfo.Board' fidl
/// protocol and for the Platform_ID and Board_Info ZBI items.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HardwareInfo {
    /// This is the value returned in the 'BoardInfo.name' field, if different
    /// from the name provided for the board itself.  It's also the name that's
    /// set in the PLATFORM_ID ZBI Item.
    pub name: Option<String>,

    /// The vendor id to add to a PLATFORM_ID ZBI Item.
    pub vendor_id: Option<u32>,

    /// The product id to add to a PLATFORM_ID ZBI Item.
    pub product_id: Option<u32>,

    /// The board revision to add to a BOARD_INFO ZBI Item.
    pub revision: Option<u32>,
}

/// This struct defines a bundle of artifacts that can be included by the board
/// in the assembled image.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, SupportsFileRelativePaths)]
#[serde(deny_unknown_fields)]
pub struct BoardInputBundle {
    /// These are the drivers that are included by this bundle.
    #[file_relative_paths]
    pub drivers: Vec<PackagedDriverDetails>,

    /// These are the packages to include with this bundle.
    #[file_relative_paths]
    pub packages: Vec<PackageDetails>,

    /// These are kernel boot arguments that are to be passed to the kernel when
    /// this bundle is included in the assembled system.
    pub kernel_boot_args: BTreeSet<String>,

    /// Board-provided configuration for platform services.  Each field of this
    /// structure can only be provided by one of the BoardInputBundles that a
    /// BoardInformation uses.
    #[file_relative_paths]
    pub configuration: Option<BoardProvidedConfig>,
}

/// This struct defines board-provided configuration for platform services and
/// features, used if those services are included by the product's supplied
/// platform configuration.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, SupportsFileRelativePaths)]
#[serde(deny_unknown_fields)]
pub struct BoardProvidedConfig {
    /// Configuration for the cpu-manager service
    #[file_relative_paths]
    pub cpu_manager: Option<FileRelativePathBuf>,

    /// Energy model configuration for processor power management
    #[file_relative_paths]
    pub energy_model: Option<FileRelativePathBuf>,

    /// Configuration for the power-manager service
    #[file_relative_paths]
    pub power_manager: Option<FileRelativePathBuf>,

    /// Configuration for the power metrics recorder service
    #[file_relative_paths]
    pub power_metrics_recorder: Option<FileRelativePathBuf>,

    /// System power modes configuration
    #[file_relative_paths]
    pub system_power_mode: Option<FileRelativePathBuf>,

    /// Thermal configuration for the power-manager service
    #[file_relative_paths]
    pub thermal: Option<FileRelativePathBuf>,

    /// These files describe performance "roles" that threads can take.  These roles translate to
    /// Zircon profiles that change the runtime properties of the thread
    #[serde(default)]
    #[file_relative_paths]
    pub thread_roles: Vec<FileRelativePathBuf>,

    /// Sysmem format costs configuration for the board. The file content bytes
    /// are a persistent fidl fuchsia.sysmem2.FormatCosts. Normally json[5]
    /// would be preferable for config, but we generate this config in rust
    /// using FIDL types (to avoid repetition and to take advantage of FIDL rust
    /// codegen), and there's no json schema for FIDL types.
    ///
    /// See BoardInformation.platform.sysmem_defaults for other board-level
    /// sysmem config.
    #[serde(default)]
    #[file_relative_paths]
    pub sysmem_format_costs: Vec<FileRelativePathBuf>,
}

/// Where to print the serial logs.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SerialMode {
    /// Do not output any serial logs.
    #[default]
    NoOutput,
    /// Output the serial logs to the legacy console.
    /// This is only valid on 'eng' builds.
    Legacy,
}

/// This struct defines supported kernel features.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct BoardKernelConfig {
    /// Enable the use of 'contiguous physical pages'. This should be enabled
    /// when a significant contiguous memory size is required.
    pub contiguous_physical_pages: bool,

    /// Where to print serial logs.
    pub serial_mode: SerialMode,

    /// Disable printing to the console during early boot (ie, make it quiet)
    pub quiet_early_boot: bool,

    /// When enabled, each ARM cpu will enable an event stream generator, which
    /// per-cpu sets the hidden event flag at a particular rate. This has the
    /// effect of kicking cpus out of any WFE states they may be sitting in.
    pub arm64_event_stream_enable: bool,

    /// This controls what serial port is used.  If provided, it overrides the
    /// serial port described by the system's bootdata.  The kernel debug serial
    /// port is a reserved resource and may not be used outside of the kernel.
    ///
    /// If set to "none", the kernel debug serial port will be disabled and will
    /// not be reserved, allowing the default serial port to be used outside the
    /// kernel.
    pub serial: Option<String>,

    /// When searching for a CPU on which to place a task, prefer little cores
    /// over big cores. Enabling this option trades off improved performance in
    /// favor of reduced power consumption.
    pub scheduler_prefer_little_cpus: bool,

    /// The system will halt on a kernel panic instead of rebooting.
    pub halt_on_panic: bool,

    /// OOM related configurations.
    pub oom: Option<OOM>,
}

impl Default for BoardKernelConfig {
    fn default() -> Self {
        Self {
            contiguous_physical_pages: false,
            serial_mode: SerialMode::default(),
            quiet_early_boot: false,
            arm64_event_stream_enable: true,
            serial: None,
            scheduler_prefer_little_cpus: false,
            halt_on_panic: false,
            oom: None,
        }
    }
}

/// This struct defines supported Out of memory features.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OOM {
    /// This option triggers eviction of file pages at the Warning pressure
    /// state, in addition to the default behavior, which is to evict at the
    /// Critical and OOM states.
    #[serde(default)]
    pub evict_at_warning: bool,

    /// This option configures kernel eviction to run continually in the
    /// background to try and keep the system out of memory pressure, as opposed
    /// to triggering one-shot eviction only at memory pressure level
    /// transitions.
    #[serde(default)]
    pub evict_continuous: bool,

    /// This option specifies the free-memory threshold at which the
    /// out-of-memory (OOM) thread will trigger an out-of-memory event and begin
    /// killing processes, or rebooting the system.
    #[serde(default)]
    pub out_of_memory_mb: Option<u32>,

    /// This option specifies the free-memory threshold at which the
    /// out-of-memory (OOM) thread will trigger a critical memory pressure
    /// event, signaling that processes should free up memory.
    #[serde(default)]
    pub critical_mb: Option<u32>,

    /// This option specifies the free-memory threshold at which the
    /// out-of-memory (OOM) thread will trigger a warning memory pressure event,
    /// signaling that processes should slow down memory allocations.
    #[serde(default)]
    pub warning_mb: Option<u32>,
}

/// This struct defines platform configurations specified by board.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, SupportsFileRelativePaths)]
#[serde(deny_unknown_fields)]
pub struct PlatformConfig {
    /// Configure connectivity related features
    #[serde(default)]
    pub connectivity: ConnectivityConfig,

    /// Configure development support related features
    #[serde(default)]
    pub development_support: DevelopmentSupportConfig,

    /// Configure development support related features
    #[serde(default)]
    pub graphics: GraphicsConfig,

    /// Sysmem board defaults. This can be overridden field-by-field by the same
    /// struct in platform config.
    ///
    /// We don't provide format_costs_persistent_fidl files via this struct, as
    /// a BoardInputBundle provides the files via the BoardProvidedConfig
    /// struct.
    #[serde(default)]
    pub sysmem_defaults: BoardSysmemConfig,
}

/// This struct defines connectivity configurations.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ConnectivityConfig {
    /// Configure network related features
    pub network: NetworkConfig,
}

/// This struct defines development support configurations.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DevelopmentSupportConfig {
    /// Configure debug access port for specific SoC
    pub enable_debug_access_port_for_soc: Option<Arm64DebugDapSoc>,
}

/// This struct defines network configurations.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NetworkConfig {
    /// This option instructs netsvc to use only the device whose topological
    /// path ends with the option's value, with any wildcard `*` characters
    /// matching any zero or more characters of the topological path. All other
    /// devices are ignored by netsvc. The topological path for a device can be
    /// determined from the shell by running the `lsdev` command on the device
    /// (e.g. `/dev/class/network/000` or `/dev/class/ethernet/000`).
    pub netsvc_interface: Option<String>,
}

/// This struct defines graphics configurations.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GraphicsConfig {
    /// Configure display related features.
    pub display: DisplayConfig,
}

/// This struct defines display configurations.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DisplayConfig {
    /// The number of degrees to the rotate the screen display by.
    pub rotation: Option<u32>,

    /// Whether the display has rounded corners.
    pub rounded_corners: Option<bool>,
}

#[cfg(test)]
mod test {
    use super::*;
    use camino::Utf8PathBuf;

    #[test]
    fn test_basic_board_deserialize() {
        let json = serde_json::json!({
            "name": "sample board",
        });

        let parsed: BoardInformation = serde_json::from_value(json).unwrap();
        let expected = BoardInformation { name: "sample board".to_owned(), ..Default::default() };

        assert_eq!(parsed, expected);
    }

    #[test]
    fn test_complete_board_deserialize_with_relative_paths() {
        let board_dir = Utf8PathBuf::from("some/path/to/board");
        let board_file = board_dir.join("board_configuration.json");

        let json = serde_json::json!({
            "name": "sample board",
            "hardware_info": {
                "name": "hwinfo_name",
                "vendor_id": 1,
                "product_id": 2,
                "revision": 3,
            },
            "provided_features": [
                "feature_a",
                "feature_b"
            ],
            "input_bundles": [
                "bundle_a",
                "bundle_b"
            ],
            "devicetree": "test.dtb",
            "kernel": {
                "contiguous_physical_pages": true,
                "scheduler_prefer_little_cpus": true,
                "arm64_event_stream_enable": false,
            },
            "platform": {
                "development_support": {
                    "enable_debug_access_port_for_soc": "amlogic-t931g",
                }
            }
        });

        let parsed: BoardInformation = serde_json::from_value(json).unwrap();
        let resolved = parsed.resolve_paths_from_file(board_file).unwrap();

        let expected = BoardInformation {
            name: "sample board".to_owned(),
            hardware_info: HardwareInfo {
                name: Some("hwinfo_name".into()),
                vendor_id: Some(0x01),
                product_id: Some(0x02),
                revision: Some(0x03),
            },
            provided_features: vec!["feature_a".into(), "feature_b".into()],
            input_bundles: vec![
                FileRelativePathBuf::Resolved("some/path/to/board/bundle_a".into()),
                FileRelativePathBuf::Resolved("some/path/to/board/bundle_b".into()),
            ],
            devicetree: Some(FileRelativePathBuf::Resolved("some/path/to/board/test.dtb".into())),
            kernel: BoardKernelConfig {
                contiguous_physical_pages: true,
                serial_mode: SerialMode::NoOutput,
                quiet_early_boot: false,
                serial: None,
                scheduler_prefer_little_cpus: true,
                halt_on_panic: false,
                oom: None,
                arm64_event_stream_enable: false,
            },
            platform: PlatformConfig {
                connectivity: ConnectivityConfig::default(),
                development_support: DevelopmentSupportConfig {
                    enable_debug_access_port_for_soc: Some(Arm64DebugDapSoc::AmlogicT931g),
                },
                graphics: GraphicsConfig::default(),
                sysmem_defaults: BoardSysmemConfig::default(),
            },
            ..Default::default()
        };

        assert_eq!(resolved, expected);
    }
}
