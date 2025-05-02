// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![deny(missing_docs)]

use std::collections::{BTreeMap, BTreeSet};

use crate::common::{PackageDetails, PackagedDriverDetails};
use crate::platform_config::sysmem_config::BoardSysmemConfig;
use anyhow::Result;
use assembly_constants::Arm64DebugDapSoc;
use assembly_container::{assembly_container, AssemblyContainer, DirectoryPathBuf, WalkPaths};
use assembly_images_config::BoardFilesystemConfig;
use camino::Utf8PathBuf;
use serde::{Deserialize, Serialize};

/// This struct provides information about the "board" that a product is being
/// assembled to run on.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, WalkPaths)]
#[serde(deny_unknown_fields)]
#[assembly_container(board_configuration.json)]
pub struct BoardInformation {
    /// The name of the board.
    pub name: String,

    /// Metadata about the board that's provided to the 'fuchsia.hwinfo.Board'
    /// protocol and to the Board Driver via the PlatformID and BoardInfo ZBI
    /// items.
    #[serde(default)]
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub hardware_info: HardwareInfo,

    /// The "features" that this board provides to the product.
    ///
    /// NOTE: This is a still-evolving, loosely-coupled, set of identifiers.
    /// It's an unstable interface between the boards and the platform.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub provided_features: Vec<String>,

    /// Path to the devicetree binary (.dtb) this provided by this board.
    #[serde(default)]
    #[walk_paths]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devicetree: Option<Utf8PathBuf>,

    /// Path to the devicetree binary overlay (.dtbo) this provided by this board.
    #[serde(default)]
    #[walk_paths]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub devicetree_overlay: Option<Utf8PathBuf>,

    /// Configuration for the various filesystems that the product can choose to
    /// include.
    #[serde(default)]
    #[walk_paths]
    #[serde(skip_serializing_if = "crate::common::is_default")]
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
    #[walk_paths]
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub input_bundles: BTreeMap<String, DirectoryPathBuf>,

    /// Consolidated configuration from all of the BoardInputBundles.  This is
    /// not deserialized from the BoardConfiguration, but is instead created by
    /// parsing each of the input_bundles and merging their configuration fields.
    #[serde(skip)]
    #[walk_paths]
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub configuration: BoardProvidedConfig,

    /// Configure kernel cmdline args
    /// TODO: Move this into platform section below
    #[serde(default)]
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub kernel: BoardKernelConfig,

    /// Configure platform related feature
    #[serde(default)]
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub platform: PlatformConfig,

    /// GUIDs for the TAs provided by this board's TEE driver.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub global_platform_tee_trusted_app_guids: Vec<uuid::Uuid>,

    /// GUIDs for the TAs provided by this board's TEE driver.
    ///
    /// NOTE: This is the deprecated name for
    /// `BoardInformation::global_platform_tee_trusted_app_guids`. At most one of the two fields
    /// may be non-empty.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tee_trusted_app_guids: Vec<uuid::Uuid>,

    /// Release version that this board config corresponds to.
    pub release_version: String,
}

/// This struct defines board-provided data for the 'fuchsia.hwinfo.Board' fidl
/// protocol and for the Platform_ID and Board_Info ZBI items.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HardwareInfo {
    /// This is the value returned in the 'BoardInfo.name' field, if different
    /// from the name provided for the board itself.  It's also the name that's
    /// set in the PLATFORM_ID ZBI Item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// The vendor id to add to a PLATFORM_ID ZBI Item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<u32>,

    /// The product id to add to a PLATFORM_ID ZBI Item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_id: Option<u32>,

    /// The board revision to add to a BOARD_INFO ZBI Item.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<u32>,
}

impl BoardInformation {
    /// Add the names of the BIBs to the map.
    pub fn add_bib_names(mut self) -> Result<Self> {
        self.input_bundles = self
            .input_bundles
            .into_values()
            .map(|dir| {
                let bib = BoardInputBundle::from_dir(&dir)?;
                Ok::<(String, DirectoryPathBuf), anyhow::Error>((bib.name, dir))
            })
            .collect::<Result<BTreeMap<String, DirectoryPathBuf>>>()?;
        Ok(self)
    }
}

/// This struct defines a bundle of artifacts that can be included by the board
/// in the assembled image.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, WalkPaths)]
#[assembly_container(board_input_bundle.json)]
#[serde(default, deny_unknown_fields)]
pub struct BoardInputBundle {
    /// The name of the board input bundle.
    pub name: String,

    /// These are the drivers that are included by this bundle.
    #[walk_paths]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub drivers: Vec<PackagedDriverDetails>,

    /// These are the packages to include with this bundle.
    #[walk_paths]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<PackageDetails>,

    /// These are kernel boot arguments that are to be passed to the kernel when
    /// this bundle is included in the assembled system.
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    pub kernel_boot_args: BTreeSet<String>,

    /// Board-provided configuration for platform services.  Each field of this
    /// structure can only be provided by one of the BoardInputBundles that a
    /// BoardInformation uses.
    #[walk_paths]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configuration: Option<BoardProvidedConfig>,

    /// Release version that this board config corresponds to.
    pub release_version: String,
}

/// This struct defines board-provided configuration for platform services and
/// features, used if those services are included by the product's supplied
/// platform configuration.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, WalkPaths)]
#[serde(deny_unknown_fields)]
pub struct BoardProvidedConfig {
    /// Configuration for the cpu-manager service
    #[walk_paths]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_manager: Option<Utf8PathBuf>,

    /// Energy model configuration for processor power management
    #[walk_paths]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub energy_model: Option<Utf8PathBuf>,

    /// Configuration for the power-manager service
    #[walk_paths]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_manager: Option<Utf8PathBuf>,

    /// Configuration for the power metrics recorder service
    #[walk_paths]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub power_metrics_recorder: Option<Utf8PathBuf>,

    /// System power modes configuration
    #[walk_paths]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_power_mode: Option<Utf8PathBuf>,

    /// Thermal configuration for the power-manager service
    #[walk_paths]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thermal: Option<Utf8PathBuf>,

    /// These files describe performance "roles" that threads can take.  These roles translate to
    /// Zircon profiles that change the runtime properties of the thread
    #[serde(default)]
    #[walk_paths]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub thread_roles: Vec<Utf8PathBuf>,

    /// Sysmem format costs configuration for the board. The file content bytes
    /// are a persistent fidl fuchsia.sysmem2.FormatCosts. Normally json[5]
    /// would be preferable for config, but we generate this config in rust
    /// using FIDL types (to avoid repetition and to take advantage of FIDL rust
    /// codegen), and there's no json schema for FIDL types.
    ///
    /// See BoardInformation.platform.sysmem_defaults for other board-level
    /// sysmem config.
    #[serde(default)]
    #[walk_paths]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sysmem_format_costs: Vec<Utf8PathBuf>,
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
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub contiguous_physical_pages: bool,

    /// Where to print serial logs.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub serial_mode: SerialMode,

    /// Disable printing to the console during early boot (ie, make it quiet)
    #[serde(skip_serializing_if = "crate::common::is_default")]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,

    /// When searching for a CPU on which to place a task, prefer little cores
    /// over big cores. Enabling this option trades off improved performance in
    /// favor of reduced power consumption.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub scheduler_prefer_little_cpus: bool,

    /// The system will halt on a kernel panic instead of rebooting.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub halt_on_panic: bool,

    /// OOM related configurations.
    #[serde(skip_serializing_if = "Option::is_none")]
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
#[serde(default, deny_unknown_fields)]
pub struct OOM {
    /// This option triggers eviction of file pages at the Warning pressure
    /// state, in addition to the default behavior, which is to evict at the
    /// Critical and OOM states.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub evict_at_warning: bool,

    /// This option configures kernel eviction to run continually in the
    /// background to try and keep the system out of memory pressure, as opposed
    /// to triggering one-shot eviction only at memory pressure level
    /// transitions.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub evict_continuous: bool,

    /// This option specifies the free-memory threshold at which the
    /// out-of-memory (OOM) thread will trigger an out-of-memory event and begin
    /// killing processes, or rebooting the system.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub out_of_memory_mb: Option<u32>,

    /// This option specifies the free-memory threshold at which the
    /// out-of-memory (OOM) thread will trigger a critical memory pressure
    /// event, signaling that processes should free up memory.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub critical_mb: Option<u32>,

    /// This option specifies the free-memory threshold at which the
    /// out-of-memory (OOM) thread will trigger a warning memory pressure event,
    /// signaling that processes should slow down memory allocations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning_mb: Option<u32>,
}

/// This struct defines platform configurations specified by board.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PlatformConfig {
    /// Configure connectivity related features
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub connectivity: ConnectivityConfig,

    /// Configure development support related features
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub development_support: DevelopmentSupportConfig,

    /// Configure development support related features
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub graphics: GraphicsConfig,

    /// Sysmem board defaults. This can be overridden field-by-field by the same
    /// struct in platform config.
    ///
    /// We don't provide format_costs_persistent_fidl files via this struct, as
    /// a BoardInputBundle provides the files via the BoardProvidedConfig
    /// struct.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub sysmem_defaults: BoardSysmemConfig,
}

/// This struct defines connectivity configurations.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ConnectivityConfig {
    /// Configure network related features
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub network: NetworkConfig,
}

/// This struct defines development support configurations.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DevelopmentSupportConfig {
    /// Configure debug access port for specific SoC
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_debug_access_port_for_soc: Option<Arm64DebugDapSoc>,
}

/// This struct defines network configurations.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct NetworkConfig {
    /// This option instructs netsvc to use only the device whose topological
    /// path ends with the option's value, with any wildcard `*` characters
    /// matching any zero or more characters of the topological path. All other
    /// devices are ignored by netsvc. The topological path for a device can be
    /// determined from the shell by running the `lsdev` command on the device
    /// (e.g. `/dev/class/network/000` or `/dev/class/ethernet/000`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub netsvc_interface: Option<String>,
}

/// This struct defines graphics configurations.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct GraphicsConfig {
    /// Configure display related features.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub display: DisplayConfig,
}

/// This struct defines display configurations.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DisplayConfig {
    /// The number of degrees to the rotate the screen display by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<u32>,

    /// Whether the display has rounded corners.
    #[serde(skip_serializing_if = "crate::common::is_default")]
    pub rounded_corners: bool,
}

#[cfg(test)]
mod test {
    use super::*;
    use camino::Utf8PathBuf;
    use tempfile::tempdir;

    #[test]
    fn test_basic_board_deserialize() {
        let json = serde_json::json!({
            "name": "sample board",
            "release_version": "",
        });

        let parsed: BoardInformation = serde_json::from_value(json).unwrap();
        let expected = BoardInformation { name: "sample board".to_owned(), ..Default::default() };

        assert_eq!(parsed, expected);
    }

    #[test]
    fn test_board_default_serialization() {
        let value: BoardInformation =
            serde_json::from_str("{\"name\": \"foo\", \"release_version\": \"\"}").unwrap();
        crate::common::tests::value_serialization_helper(value);
    }

    #[test]
    fn test_bib_default_serialization() {
        crate::common::tests::default_serialization_helper::<BoardInputBundle>();
    }

    #[test]
    fn test_board_provided_config_default_serialization() {
        crate::common::tests::default_serialization_helper::<BoardProvidedConfig>();
    }

    #[test]
    fn test_board_kernel_config_default_serialization() {
        crate::common::tests::default_serialization_helper::<BoardKernelConfig>();
    }

    #[test]
    fn test_complete_board_deserialize_with_relative_paths() {
        let dir = tempdir().unwrap();
        let dir_path = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();

        let config_path = dir_path.join("board_configuration.json");
        let config_file = std::fs::File::create(&config_path).unwrap();

        let devicetree_path = dir_path.join("test.dtb");
        std::fs::write(&devicetree_path, "").unwrap();

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
            "input_bundles": {},
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
            },
            "release_version": "fake_version_123"
        });
        serde_json::to_writer(config_file, &json).unwrap();
        let resolved = BoardInformation::from_dir(&dir_path).unwrap();

        let expected = BoardInformation {
            name: "sample board".to_owned(),
            hardware_info: HardwareInfo {
                name: Some("hwinfo_name".into()),
                vendor_id: Some(0x01),
                product_id: Some(0x02),
                revision: Some(0x03),
            },
            provided_features: vec!["feature_a".into(), "feature_b".into()],
            input_bundles: [].into(),
            devicetree: Some(devicetree_path),
            devicetree_overlay: None,
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
            release_version: "fake_version_123".to_string(),
            ..Default::default()
        };

        assert_eq!(resolved, expected);
    }
}
