// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_fxfs::{BlobCreatorMarker, BlobReaderMarker};
use fuchsia_component_test::{Capability, ChildOptions, ChildRef, RealmBuilder, Ref, Route};
use std::collections::HashMap;

use {
    fidl_fuchsia_fshost as ffshost, fidl_fuchsia_fxfs as ffxfs, fidl_fuchsia_io as fio,
    fidl_fuchsia_logger as flogger, fidl_fuchsia_process as fprocess,
    fidl_fuchsia_storage_partitions as fpartitions, fidl_fuchsia_update_verify as ffuv,
};

pub trait IntoValueSpec {
    fn into_value_spec(self) -> cm_rust::ConfigValueSpec;
}

impl IntoValueSpec for bool {
    fn into_value_spec(self) -> cm_rust::ConfigValueSpec {
        cm_rust::ConfigValueSpec {
            value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::Bool(self)),
        }
    }
}

impl IntoValueSpec for u64 {
    fn into_value_spec(self) -> cm_rust::ConfigValueSpec {
        cm_rust::ConfigValueSpec {
            value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::Uint64(self)),
        }
    }
}

impl IntoValueSpec for String {
    fn into_value_spec(self) -> cm_rust::ConfigValueSpec {
        cm_rust::ConfigValueSpec {
            value: cm_rust::ConfigValue::Single(cm_rust::ConfigSingleValue::String(self)),
        }
    }
}

impl<'a> IntoValueSpec for &'a str {
    fn into_value_spec(self) -> cm_rust::ConfigValueSpec {
        self.to_string().into_value_spec()
    }
}

/// Builder for the fshost component. This handles configuring the fshost component to use and
/// structured config overrides to set, as well as setting up the expected protocols to be routed
/// between the realm builder root and the fshost child when the test realm is built.
///
/// Any desired additional config overrides should be added to this builder. New routes for exposed
/// capabilities from the fshost component or offered capabilities to the fshost component should
/// be added to the [`FshostBuilder::build`] function below.
#[derive(Debug, Clone)]
pub struct FshostBuilder {
    component_name: &'static str,
    config_values: HashMap<&'static str, cm_rust::ConfigValueSpec>,
}

impl FshostBuilder {
    pub fn new(component_name: &'static str) -> FshostBuilder {
        FshostBuilder { component_name, config_values: HashMap::new() }
    }

    pub fn set_config_value(&mut self, key: &'static str, value: impl IntoValueSpec) -> &mut Self {
        assert!(
            self.config_values.insert(key, value.into_value_spec()).is_none(),
            "Attempted to insert duplicate config value '{}'!",
            key
        );
        self
    }

    pub(crate) async fn build(mut self, realm_builder: &RealmBuilder) -> ChildRef {
        let fshost_url = format!("#meta/{}.cm", self.component_name);
        log::info!(fshost_url:%; "building test fshost instance");
        let fshost = realm_builder
            .add_child("test-fshost", fshost_url, ChildOptions::new().eager())
            .await
            .unwrap();

        // This is a map from config keys to configuration capability names.
        let mut map = HashMap::from([
            ("no_zxcrypt", "fuchsia.fshost.NoZxcrypt"),
            ("ramdisk_image", "fuchsia.fshost.RamdiskImage"),
            ("gpt_all", "fuchsia.fshost.GptAll"),
            ("check_filesystems", "fuchsia.fshost.CheckFilesystems"),
            ("blobfs_max_bytes", "fuchsia.fshost.BlobfsMaxBytes"),
            ("data_max_bytes", "fuchsia.fshost.DataMaxBytes"),
            ("format_data_on_corruption", "fuchsia.fshost.FormatDataOnCorruption"),
            ("data_filesystem_format", "fuchsia.fshost.DataFilesystemFormat"),
            ("nand", "fuchsia.fshost.Nand"),
            ("blobfs", "fuchsia.fshost.Blobfs"),
            ("bootpart", "fuchsia.fshost.BootPart"),
            ("factory", "fuchsia.fshost.Factory"),
            ("fvm", "fuchsia.fshost.Fvm"),
            ("gpt", "fuchsia.fshost.Gpt"),
            ("mbr", "fuchsia.fshost.Mbr"),
            ("data", "fuchsia.fshost.Data"),
            ("netboot", "fuchsia.fshost.Netboot"),
            ("use_disk_migration", "fuchsia.fshost.UseDiskMigration"),
            ("disable_block_watcher", "fuchsia.fshost.DisableBlockWatcher"),
            ("fvm_slice_size", "fuchsia.fshost.FvmSliceSize"),
            ("blobfs_initial_inodes", "fuchsia.fshost.BlobfsInitialInodes"),
            (
                "blobfs_use_deprecated_padded_format",
                "fuchsia.fshost.BlobfsUseDeprecatedPaddedFormat",
            ),
            ("fxfs_blob", "fuchsia.fshost.FxfsBlob"),
            ("fxfs_crypt_url", "fuchsia.fshost.FxfsCryptUrl"),
            ("storage_host", "fuchsia.fshost.StorageHost"),
            ("disable_automount", "fuchsia.fshost.DisableAutomount"),
            ("starnix_volume_name", "fuchsia.fshost.StarnixVolumeName"),
            ("blobfs_write_compression_algorithm", "fuchsia.blobfs.WriteCompressionAlgorithm"),
            ("blobfs_cache_eviction_policy", "fuchsia.blobfs.CacheEvictionPolicy"),
        ]);

        // Note that the `starnix_volume_name` config should not be set in the fshost integration
        // test package because the builder is not aware of those and won't know to setup the crypt
        // service.
        if self.config_values.contains_key("starnix_volume_name") {
            let user_fxfs_crypt = realm_builder
                .add_child("user_fxfs_crypt", "#meta/fxfs-crypt.cm", ChildOptions::new().eager())
                .await
                .unwrap();
            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::protocol::<ffxfs::CryptMarker>())
                        .capability(Capability::protocol::<ffxfs::CryptManagementMarker>())
                        .from(&user_fxfs_crypt)
                        .to(Ref::parent()),
                )
                .await
                .unwrap();
        }

        // Add the overrides as capabilities and route them.
        self.config_values.insert("fxfs_crypt_url", "#meta/fxfs-crypt.cm".into_value_spec());
        for (key, value) in self.config_values {
            let cap_name = map[key];
            realm_builder
                .add_capability(cm_rust::CapabilityDecl::Config(cm_rust::ConfigurationDecl {
                    name: cap_name.parse().unwrap(),
                    value: value.value,
                }))
                .await
                .unwrap();
            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::configuration(cap_name))
                        .from(Ref::self_())
                        .to(&fshost),
                )
                .await
                .unwrap();
            map.remove(key);
        }

        // Add the remaining keys from the config component.
        let fshost_config_url = format!("#meta/{}_config.cm", self.component_name);
        let fshost_config = realm_builder
            .add_child("test-fshost-config", fshost_config_url, ChildOptions::new().eager())
            .await
            .unwrap();
        for (_, value) in map.iter() {
            realm_builder
                .add_route(
                    Route::new()
                        .capability(Capability::configuration(*value))
                        .from(&fshost_config)
                        .to(&fshost),
                )
                .await
                .unwrap();
        }

        realm_builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<ffshost::AdminMarker>())
                    .capability(Capability::protocol::<ffshost::RecoveryMarker>())
                    .capability(Capability::protocol::<ffuv::BlobfsVerifierMarker>())
                    .capability(Capability::protocol::<ffuv::ComponentOtaHealthCheckMarker>())
                    .capability(Capability::protocol::<ffshost::StarnixVolumeProviderMarker>())
                    .capability(Capability::protocol::<fpartitions::PartitionsManagerMarker>())
                    .capability(Capability::protocol::<BlobCreatorMarker>())
                    .capability(Capability::protocol::<BlobReaderMarker>())
                    .capability(Capability::directory("blob").rights(fio::RW_STAR_DIR))
                    .capability(Capability::directory("data").rights(fio::RW_STAR_DIR))
                    .capability(Capability::directory("tmp").rights(fio::RW_STAR_DIR))
                    .capability(Capability::directory("volumes").rights(fio::RW_STAR_DIR))
                    .capability(Capability::service::<fpartitions::PartitionServiceMarker>())
                    .from(&fshost)
                    .to(Ref::parent()),
            )
            .await
            .unwrap();

        realm_builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<flogger::LogSinkMarker>())
                    .capability(Capability::protocol::<fprocess::LauncherMarker>())
                    .from(Ref::parent())
                    .to(&fshost),
            )
            .await
            .unwrap();

        realm_builder
            .add_route(
                Route::new()
                    .capability(
                        Capability::protocol_by_name("fuchsia.scheduler.RoleManager").optional(),
                    )
                    .capability(
                        Capability::protocol_by_name("fuchsia.tracing.provider.Registry")
                            .optional(),
                    )
                    .capability(
                        Capability::protocol_by_name("fuchsia.kernel.VmexResource").optional(),
                    )
                    .capability(
                        Capability::protocol_by_name("fuchsia.memorypressure.Provider").optional(),
                    )
                    .from(Ref::void())
                    .to(&fshost),
            )
            .await
            .unwrap();

        fshost
    }
}
