// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::DomainConfigDirectoryBuilder;
use crate::subsystems::prelude::*;
use anyhow::{anyhow, Context};
use assembly_component_id_index::{ComponentIdIndexBuilder, Index};
use assembly_config_capabilities::{Config, ConfigNestedValueType, ConfigValueType};
use assembly_config_schema::platform_config::diagnostics_config::{
    ArchivistConfig, ArchivistPipeline, DiagnosticsConfig, FireConfig, PipelineType, SamplerConfig,
    Severity,
};
use assembly_config_schema::platform_config::storage_config::StorageConfig;
use assembly_constants::{
    BootfsDestination, BootfsPackageDestination, FileEntry, PackageSetDestination,
};
use assembly_util::read_config;
use camino::{Utf8Path, Utf8PathBuf};
use sampler_config::assembly::{
    ComponentIdInfoList, MergedSamplerConfig, ProjectConfig, ProjectTemplate,
};
use std::collections::BTreeSet;

const ALLOWED_SERIAL_LOG_COMPONENTS: &[&str] = &[
    "/bootstrap/**",
    "/core/mdns",
    "/core/network/netcfg",
    "/core/network/netstack",
    "/core/sshd-host",
    "/core/system-update/system-update-committer",
    "/core/wlancfg",
    "/core/wlandevicemonitor",
];

const DENIED_SERIAL_LOG_TAGS: &[&str] = &["NUD"];

pub(crate) struct DiagnosticsSubsystemConfig<'a> {
    pub diagnostics: &'a DiagnosticsConfig,
    pub storage: &'a StorageConfig,
}

pub(crate) struct DiagnosticsSubsystem;
impl<'a> DefineSubsystemConfiguration<DiagnosticsSubsystemConfig<'a>> for DiagnosticsSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &DiagnosticsSubsystemConfig<'a>,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // Unconditionally include console AIB for now. In the future, we may add an option to
        // disable this.
        builder.platform_bundle("console");
        if context.build_type == &BuildType::User {
            builder.platform_bundle("detect_user");
        }

        let DiagnosticsConfig {
            archivist,
            archivist_pipelines,
            additional_serial_log_components,
            sampler,
            memory_monitor,
            component_log_initial_interests,
        } = config.diagnostics;
        // LINT.IfChange
        let mut bind_services = BTreeSet::from([
            "fuchsia.component.PersistenceBinder",
            "fuchsia.component.SamplerBinder",
        ]);
        // fuchsia.diagnostics.MaximumConcurrentSnapshotsPerReader default:
        let mut maximum_concurrent_snapshots_per_reader = 4;
        // fuchsia.diagnostics.LogsMaxCachedOriginalBytes default:
        let mut logs_max_cached_original_bytes = 4194304;

        match (context.build_type, context.feature_set_level) {
            // Always clear bind_services for bootstrap (bringup) and utility
            // systems.
            (_, FeatureSetLevel::Bootstrap)
            | (_, FeatureSetLevel::Embeddable)
            | (_, FeatureSetLevel::Utility) => {
                bind_services.clear();
            }
            // Some services aren't present on user builds.
            (BuildType::User, FeatureSetLevel::Standard) => {}
            (_, FeatureSetLevel::Standard) => {
                bind_services.insert("fuchsia.component.DetectBinder");
                bind_services.insert("fuchsia.component.KernelDebugBrokerBinder");
            }
        };

        match archivist {
            Some(ArchivistConfig::Default) | None => {}
            Some(ArchivistConfig::LowMem) => {
                logs_max_cached_original_bytes = 2097152;
                maximum_concurrent_snapshots_per_reader = 2;
            }
        }

        let mut allow_serial_logs: BTreeSet<String> =
            ALLOWED_SERIAL_LOG_COMPONENTS.iter().map(|ref s| s.to_string()).collect();
        allow_serial_logs
            .extend(additional_serial_log_components.iter().map(|ref s| s.to_string()));
        let allow_serial_logs: Vec<String> = allow_serial_logs.into_iter().collect();
        let deny_serial_log_tags: Vec<String> =
            DENIED_SERIAL_LOG_TAGS.iter().map(|ref s| s.to_string()).collect();

        builder.set_config_capability(
            "fuchsia.diagnostics.BindServices",
            Config::new(
                ConfigValueType::Vector {
                    nested_type: ConfigNestedValueType::String { max_size: 256 },
                    max_count: 10,
                },
                bind_services.into_iter().collect::<Vec<_>>().into(),
            ),
        )?;
        builder.set_config_capability(
            "fuchsia.diagnostics.LogsMaxCachedOriginalBytes",
            Config::new(ConfigValueType::Uint64, logs_max_cached_original_bytes.into()),
        )?;
        builder.set_config_capability(
            "fuchsia.diagnostics.MaximumConcurrentSnapshotsPerReader",
            Config::new(ConfigValueType::Uint64, maximum_concurrent_snapshots_per_reader.into()),
        )?;
        builder.set_config_capability(
            "fuchsia.diagnostics.AllowSerialLogs",
            // LINT.ThenChange(/src/diagnostics/archivist/configs.gni)
            Config::new(
                ConfigValueType::Vector {
                    nested_type: ConfigNestedValueType::String { max_size: 50 },
                    max_count: 512,
                },
                allow_serial_logs.into(),
            ),
        )?;
        builder.set_config_capability(
            "fuchsia.diagnostics.DenySerialLogs",
            Config::new(
                ConfigValueType::Vector {
                    nested_type: ConfigNestedValueType::String { max_size: 50 },
                    max_count: 512,
                },
                deny_serial_log_tags.into(),
            ),
        )?;

        if *context.build_type == BuildType::User
            && component_log_initial_interests
                .iter()
                .any(|interest| matches!(interest.log_severity, Severity::Debug | Severity::Trace))
        {
            return Err(anyhow!(
                "Component log severity cannot be below info when build type is set to user"
            ));
        }

        builder.set_config_capability(
            "fuchsia.diagnostics.ComponentInitialInterests",
            Config::new(
                ConfigValueType::Vector {
                    nested_type: ConfigNestedValueType::String { max_size: 4096 },
                    max_count: 512,
                },
                serde_json::to_value(
                    component_log_initial_interests
                        .iter()
                        .map(|initial_interest| initial_interest.for_structured_config())
                        .collect::<Vec<_>>(),
                )?,
            ),
        )?;

        let pipelines = builder
            .add_domain_config(PackageSetDestination::Boot(
                BootfsPackageDestination::ArchivistPipelines,
            ))
            .directory("config");
        let mut saw_feedback = false;
        for pipeline in archivist_pipelines {
            let ArchivistPipeline { name, files } = pipeline;
            saw_feedback |= matches!(name, PipelineType::Feedback);
            // TODO(https://fxbug.dev/342194194): improve how we handle disabling pipelines. This
            // could probably be part of the structured configuration instead of a magic file.
            if files.is_empty() {
                insert_disabled(pipelines, name)?;
            } else {
                for file in files {
                    let filename = file.file_name().ok_or_else(|| {
                        anyhow!("Failed to get filename for archivist pipeline: {}", &file)
                    })?;
                    pipelines.entry(FileEntry {
                        source: file.clone(),
                        destination: format!("{name}/{filename}"),
                    })?;
                }
            }
        }
        // TODO(https://fxbug.dev/342194194): enable removing the "all" pipeline in user builds.
        // This behavior is currently not implemented, but should.
        // TODO(https://fxbug.dev/342194194): Feedback being empty on a non-user product means a
        // different thing than other pipelines. Other pieplines would be disabled, but feedback
        // isn't filtered. That configuration should be moved here instead of Archivist.
        if !saw_feedback {
            insert_disabled(pipelines, &PipelineType::Feedback)?;
        }

        let exception_handler_available = matches!(
            context.feature_set_level,
            FeatureSetLevel::Utility | FeatureSetLevel::Standard
        );
        builder.set_config_capability(
            "fuchsia.diagnostics.ExceptionHandlerAvailable",
            Config::new(ConfigValueType::Bool, exception_handler_available.into()),
        )?;

        match context.feature_set_level {
            FeatureSetLevel::Bootstrap | FeatureSetLevel::Utility | FeatureSetLevel::Embeddable => {
            }
            FeatureSetLevel::Standard => {
                if context.board_info.provides_feature("fuchsia::mali_gpu") {
                    builder.platform_bundle("diagnostics_triage_detect_mali");
                }
            }
        }

        // Build the component id index and add it as a bootfs file.
        let gendir = context.get_gendir().context("Getting gendir for diagnostics subsystem")?;
        let (instance_id_index, index_path) =
            build_instance_id_index(context, config.storage, gendir)?;
        builder
            .bootfs()
            .file(FileEntry {
                destination: BootfsDestination::ComponentIdIndex,
                source: index_path.clone(),
            })
            .with_context(|| format!("Adding bootfs file {}", &index_path))?;

        let default_sampler_config: MergedSamplerConfig =
            read_config(context.get_resource("default_sampler_config.json5"))?;

        let project_configs =
            load_project_configs(sampler, default_sampler_config, &instance_id_index)?
                .into_iter()
                .map(|project_config| {
                    serde_json::to_string(&project_config)
                        .context("failed to serialize project config")
                })
                .collect::<Result<Vec<String>, _>>()?;

        // LINT.IfChange
        builder.set_config_capability(
            "fuchsia.diagnostics.sampler.ProjectConfigs",
            Config::new(
                ConfigValueType::Vector {
                    nested_type: ConfigNestedValueType::String { max_size: 100000 },
                    max_count: 1024,
                },
                project_configs.into(),
            ),
        )?;
        // LINT.ThenChange(//src/diagnostics/sampler/meta/sampler.cml)

        builder.set_config_capability(
            "fuchsia.memory.CaptureOnPressureChange",
            Config::new(ConfigValueType::Bool, memory_monitor.capture_on_pressure_change.into()),
        )?;

        builder.set_config_capability(
            "fuchsia.memory.ImminentOomCaptureDelay",
            Config::new(
                ConfigValueType::Uint32,
                memory_monitor.imminent_oom_capture_delay_s.into(),
            ),
        )?;

        builder.set_config_capability(
            "fuchsia.memory.CriticalCaptureDelay",
            Config::new(ConfigValueType::Uint32, memory_monitor.critical_capture_delay_s.into()),
        )?;

        builder.set_config_capability(
            "fuchsia.memory.WarningCaptureDelay",
            Config::new(ConfigValueType::Uint32, memory_monitor.warning_capture_delay_s.into()),
        )?;

        builder.set_config_capability(
            "fuchsia.memory.NormalCaptureDelay",
            Config::new(ConfigValueType::Uint32, memory_monitor.normal_capture_delay_s.into()),
        )?;

        Ok(())
    }
}

fn load_project_configs(
    sampler: &SamplerConfig,
    default_config: MergedSamplerConfig,
    instance_id_index: &Index,
) -> anyhow::Result<Vec<ProjectConfig>> {
    let MergedSamplerConfig { mut project_configs, fire_project_templates, fire_component_configs } =
        default_config;
    let SamplerConfig { project_configs: additional_project_configs, fire: fire_config, .. } =
        sampler;

    project_configs.reserve(fire_project_templates.len());
    for project_config_path in additional_project_configs {
        // NOTE: instead of requiring files, we could require the config directly.
        let config: ProjectConfig = read_config(project_config_path).with_context(|| {
            format!("failed to read sampler config file: {project_config_path}")
        })?;
        project_configs.push(config);
    }
    let (fire_project_templates, fire_component_configs) = load_fire_configs(
        fire_project_templates,
        fire_component_configs,
        fire_config,
        instance_id_index,
    )?;

    for fire_template in fire_project_templates {
        let config = fire_template
            .expand(&fire_component_configs)
            .context("failed to expand fire template")?;
        project_configs.push(config);
    }
    Ok(project_configs)
}

fn load_fire_configs(
    mut fire_templates: Vec<ProjectTemplate>,
    default_fire_components: Vec<ComponentIdInfoList>,
    fire_config: &FireConfig,
    instance_id_index: &Index,
) -> anyhow::Result<(Vec<ProjectTemplate>, ComponentIdInfoList)> {
    let FireConfig { component_configs, project_templates } = fire_config;

    for project_template_path in project_templates {
        let config: ProjectTemplate = read_config(project_template_path).with_context(|| {
            format!("failed to read sampler fire config file: {project_template_path}")
        })?;
        fire_templates.push(config);
    }

    let mut fire_components = Vec::new();
    fire_components.extend(default_fire_components.into_iter().flat_map(|c| c.into_iter()));
    for component_config_path in component_configs {
        let config: ComponentIdInfoList =
            read_config(component_config_path).with_context(|| {
                format!("failed to read fire components config file: {component_config_path}")
            })?;
        fire_components.extend(config.into_iter());
    }

    let mut fire_components = ComponentIdInfoList::new(fire_components);
    fire_components.add_instance_ids(instance_id_index);
    Ok((fire_templates, fire_components))
}

fn build_instance_id_index(
    context: &ConfigurationContext<'_>,
    storage_config: &StorageConfig,
    gendir: impl AsRef<Utf8Path>,
) -> anyhow::Result<(Index, Utf8PathBuf)> {
    // Build and add the component id index.
    let mut index_builder = ComponentIdIndexBuilder::default();

    // Find the default platform id index and add it to the builder.
    // The "resources" directory is built and shipped alonside the platform
    // AIBs which is how it becomes available to subsystems.
    let core_index = context.get_resource("core_component_id_index.json5");
    index_builder.index(core_index);

    // If the product provided their own index, add it to the builder.
    if let Some(product_index) = &storage_config.component_id_index.product_index {
        index_builder.index(product_index);
    }
    index_builder.build(gendir).context("Building component id index")
}

fn insert_disabled<T>(pipelines: &mut T, name: &PipelineType) -> anyhow::Result<()>
where
    T: DomainConfigDirectoryBuilder + ?Sized,
{
    let destination = format!("{name}/DISABLE_FILTERING.txt");
    pipelines.entry_from_contents(
        &destination,
        concat!(
            "The presence of this file in a pipeline config directory for the Archivist indicates ",
            "that the Archivist should disable filtering by static selectors for this pipeline.",
        ),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::ConfigurationBuilderImpl;
    use assembly_config_schema::platform_config::diagnostics_config::{
        ComponentInitialInterest, SamplerConfig, UrlOrMoniker,
    };
    use camino::Utf8PathBuf;
    use sampler_config::runtime::MetricConfig;
    use sampler_config::{CustomerId, EventCode, MetricId, MetricType, ProjectId};
    use serde_json::{json, Number, Value};
    use std::fs::File;
    use tempfile::TempDir;

    struct ResourceDir(TempDir);

    impl ResourceDir {
        fn new() -> Self {
            let temp_dir = TempDir::new().unwrap();
            let this = Self(temp_dir);
            this.write(
                "core_component_id_index.json5",
                json!({
                    "instances": [
                        {
                          "instance_id": "1111111111111111111111111111111111111111111111111111111111111111",
                          "moniker": "/core/foo",
                        },
                        {
                          "instance_id": "2222222222222222222222222222222222222222222222222222222222222222",
                          "moniker": "/core/bar",
                        },
                    ]
                }),
            );
            this.write(
                "default_sampler_config.json5",
                json!({
                    "fire_project_templates": [
                        {
                            "metrics": [
                                {
                                    "metric_id": 3,
                                    "metric_type": "Integer",
                                    "selector": "component:root/samples:{INSTANCE_ID}",
                                    "upload_once": true,
                                },
                            ],
                            "poll_rate_sec": 3,
                            "project_id": 13,
                        }
                    ],
                    "fire_component_configs": [
                       json!([
                           {
                               "id": 1,
                               "label": "Foo",
                               "moniker": "/core/foo",
                           },
                           {
                               "id": 2,
                               "label": "Bar",
                               "moniker": "/core/bar",
                           },
                       ]),
                    ],
                    "project_configs": [
                        {
                            "metrics": [
                                {
                                    "event_codes": [
                                        0,
                                        1
                                    ],
                                    "metric_id": 104,
                                    "metric_type": "Occurrence",
                                    "selector": "single_counter:root/samples:counter"
                                }
                            ],
                            "poll_rate_sec": 3000,
                            "project_id": 5
                        }
                    ],
                }),
            );
            this
        }

        fn write(&self, filename: &str, content: serde_json::Value) -> Utf8PathBuf {
            let path = self.path().join(filename);
            let mut file = File::create(&path).unwrap();
            serde_json::to_writer(&mut file, &content).unwrap();
            path
        }

        fn path(&self) -> Utf8PathBuf {
            Utf8PathBuf::from_path_buf(self.0.path().to_path_buf()).unwrap()
        }
    }

    #[test]
    fn test_define_configuration_default() {
        let resource_dir = ResourceDir::new();
        let context = ConfigurationContext {
            feature_set_level: &FeatureSetLevel::Standard,
            build_type: &BuildType::Eng,
            resource_dir: resource_dir.path(),
            ..ConfigurationContext::default_for_tests()
        };
        let diagnostics =
            DiagnosticsConfig { archivist: Some(ArchivistConfig::Default), ..Default::default() };
        let mut builder = ConfigurationBuilderImpl::default();

        DiagnosticsSubsystem::define_configuration(
            &context,
            &DiagnosticsSubsystemConfig {
                diagnostics: &diagnostics,
                storage: &StorageConfig::default(),
            },
            &mut builder,
        )
        .unwrap();
        let config = builder.build();
        assert_eq!(
            config.configuration_capabilities["fuchsia.diagnostics.BindServices"].value(),
            Value::Array(vec![
                "fuchsia.component.DetectBinder".into(),
                "fuchsia.component.KernelDebugBrokerBinder".into(),
                "fuchsia.component.PersistenceBinder".into(),
                "fuchsia.component.SamplerBinder".into(),
            ])
        );
        assert_eq!(
            config.configuration_capabilities["fuchsia.diagnostics.LogsMaxCachedOriginalBytes"]
                .value(),
            Value::Number(Number::from(4194304))
        );
        assert_eq!(
            config.configuration_capabilities
                ["fuchsia.diagnostics.MaximumConcurrentSnapshotsPerReader"]
                .value(),
            Value::Number(Number::from(4))
        );
        assert_eq!(
            config.configuration_capabilities["fuchsia.diagnostics.AllowSerialLogs"].value(),
            Value::Array(ALLOWED_SERIAL_LOG_COMPONENTS.iter().cloned().map(Into::into).collect())
        );
        assert_eq!(
            config.configuration_capabilities["fuchsia.diagnostics.DenySerialLogs"].value(),
            Value::Array(DENIED_SERIAL_LOG_TAGS.iter().cloned().map(Into::into).collect())
        );
    }

    #[test]
    fn test_define_configuration_additional_serial_log_components() {
        let resource_dir = ResourceDir::new();
        let context = ConfigurationContext {
            feature_set_level: &FeatureSetLevel::Standard,
            build_type: &BuildType::Eng,
            resource_dir: resource_dir.path(),
            ..ConfigurationContext::default_for_tests()
        };
        let diagnostics = DiagnosticsConfig {
            additional_serial_log_components: vec!["/core/foo".to_string()],
            ..DiagnosticsConfig::default()
        };
        let mut builder = ConfigurationBuilderImpl::default();

        DiagnosticsSubsystem::define_configuration(
            &context,
            &DiagnosticsSubsystemConfig {
                diagnostics: &diagnostics,
                storage: &StorageConfig::default(),
            },
            &mut builder,
        )
        .unwrap();
        let config = builder.build();

        let mut serial_log_components = BTreeSet::from_iter(["/core/foo".to_string()]);
        serial_log_components
            .extend(ALLOWED_SERIAL_LOG_COMPONENTS.iter().map(|ref s| s.to_string()));
        assert_eq!(
            config.configuration_capabilities["fuchsia.diagnostics.AllowSerialLogs"].value(),
            Value::Array(serial_log_components.iter().cloned().map(Into::into).collect())
        );
    }

    #[test]
    fn test_define_configuration_low_mem() {
        let resource_dir = ResourceDir::new();
        let context = ConfigurationContext {
            feature_set_level: &FeatureSetLevel::Standard,
            build_type: &BuildType::Eng,
            resource_dir: resource_dir.path(),
            ..ConfigurationContext::default_for_tests()
        };
        let diagnostics =
            DiagnosticsConfig { archivist: Some(ArchivistConfig::LowMem), ..Default::default() };
        let mut builder = ConfigurationBuilderImpl::default();

        DiagnosticsSubsystem::define_configuration(
            &context,
            &DiagnosticsSubsystemConfig {
                diagnostics: &diagnostics,
                storage: &StorageConfig::default(),
            },
            &mut builder,
        )
        .unwrap();
        let config = builder.build();

        assert_eq!(
            config.configuration_capabilities["fuchsia.diagnostics.LogsMaxCachedOriginalBytes"]
                .value(),
            Value::Number(Number::from(2097152))
        );
        assert_eq!(
            config.configuration_capabilities
                ["fuchsia.diagnostics.MaximumConcurrentSnapshotsPerReader"]
                .value(),
            Value::Number(Number::from(2))
        );
    }

    #[test]
    fn test_default_on_bootstrap() {
        let resource_dir = ResourceDir::new();
        let context = ConfigurationContext {
            feature_set_level: &FeatureSetLevel::Bootstrap,
            build_type: &BuildType::Eng,
            resource_dir: resource_dir.path(),
            ..ConfigurationContext::default_for_tests()
        };
        let mut builder = ConfigurationBuilderImpl::default();

        DiagnosticsSubsystem::define_configuration(
            &context,
            &DiagnosticsSubsystemConfig {
                diagnostics: &DiagnosticsConfig::default(),
                storage: &StorageConfig::default(),
            },
            &mut builder,
        )
        .unwrap();
        let config = builder.build();

        assert_eq!(
            config.configuration_capabilities["fuchsia.diagnostics.BindServices"].value(),
            Value::Array(Vec::new())
        );
    }

    #[test]
    fn test_default_for_user() {
        let resource_dir = ResourceDir::new();
        let context = ConfigurationContext {
            feature_set_level: &FeatureSetLevel::Standard,
            build_type: &BuildType::User,
            resource_dir: resource_dir.path(),
            ..ConfigurationContext::default_for_tests()
        };
        let mut builder = ConfigurationBuilderImpl::default();

        DiagnosticsSubsystem::define_configuration(
            &context,
            &DiagnosticsSubsystemConfig {
                diagnostics: &DiagnosticsConfig::default(),
                storage: &StorageConfig::default(),
            },
            &mut builder,
        )
        .unwrap();
        let config = builder.build();
        assert_eq!(
            config.configuration_capabilities["fuchsia.diagnostics.BindServices"].value(),
            Value::Array(vec![
                "fuchsia.component.PersistenceBinder".into(),
                "fuchsia.component.SamplerBinder".into(),
            ])
        );
    }

    #[test]
    fn test_fire_config() {
        let resource_dir = ResourceDir::new();
        let fire_config_path = resource_dir.write(
            "fire_config.json",
            json!([
                    {
                        "id": 1234,
                        "label": "my_label",
                        "moniker": "my_moniker",
                    }
            ]),
        );

        let context = ConfigurationContext {
            feature_set_level: &FeatureSetLevel::Standard,
            build_type: &BuildType::User,
            resource_dir: resource_dir.path(),
            ..ConfigurationContext::default_for_tests()
        };
        let diagnostics = DiagnosticsConfig {
            sampler: SamplerConfig {
                fire: FireConfig {
                    component_configs: vec![fire_config_path],
                    project_templates: vec![],
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let mut builder = ConfigurationBuilderImpl::default();
        assert!(DiagnosticsSubsystem::define_configuration(
            &context,
            &DiagnosticsSubsystemConfig {
                diagnostics: &diagnostics,
                storage: &StorageConfig::default()
            },
            &mut builder
        )
        .is_ok());
    }

    #[test]
    fn test_invalid_fire_config() {
        let resource_dir = ResourceDir::new();
        let fire_config_path = resource_dir.write(
            "fire_config.json",
            json!({
                "invalid": []
            }),
        );

        let context = ConfigurationContext {
            feature_set_level: &FeatureSetLevel::Standard,
            build_type: &BuildType::User,
            resource_dir: resource_dir.path(),
            ..ConfigurationContext::default_for_tests()
        };
        let diagnostics = DiagnosticsConfig {
            sampler: SamplerConfig {
                fire: FireConfig {
                    project_templates: vec![fire_config_path],
                    component_configs: vec![],
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let mut builder = ConfigurationBuilderImpl::default();
        assert!(DiagnosticsSubsystem::define_configuration(
            &context,
            &DiagnosticsSubsystemConfig {
                diagnostics: &diagnostics,
                storage: &StorageConfig::default()
            },
            &mut builder
        )
        .is_err());
    }

    #[test]
    fn test_define_configuration_initial_log_interests() {
        let resource_dir = ResourceDir::new();
        let context = ConfigurationContext {
            feature_set_level: &FeatureSetLevel::Standard,
            build_type: &BuildType::Eng,
            resource_dir: resource_dir.path(),
            ..ConfigurationContext::default_for_tests()
        };
        let diagnostics = DiagnosticsConfig {
            component_log_initial_interests: vec![
                ComponentInitialInterest {
                    component: UrlOrMoniker::Url(
                        "fuchsia-pkg://fuchsia.com/foo#meta/bar.cm".into(),
                    ),
                    log_severity: Severity::Debug,
                },
                ComponentInitialInterest {
                    component: UrlOrMoniker::Moniker("core/coll:foo/bar".into()),
                    log_severity: Severity::Warn,
                },
            ],
            ..Default::default()
        };
        let mut builder = ConfigurationBuilderImpl::default();

        DiagnosticsSubsystem::define_configuration(
            &context,
            &DiagnosticsSubsystemConfig {
                diagnostics: &diagnostics,
                storage: &StorageConfig::default(),
            },
            &mut builder,
        )
        .unwrap();
        let config = builder.build();
        assert_eq!(
            config.configuration_capabilities["fuchsia.diagnostics.ComponentInitialInterests"]
                .value(),
            Value::Array(vec![
                "fuchsia-pkg://fuchsia.com/foo#meta/bar.cm:DEBUG".into(),
                "core/coll:foo/bar:WARN".into(),
            ])
        );
    }

    #[test]
    fn test_load_smapler_config() {
        let resource_dir = ResourceDir::new();
        let fire_test_template = resource_dir.write(
            "test_fire_template.json",
            json!({
                "metrics": [
                    {
                        "metric_id": 1,
                        "metric_type": "IntHistogram",
                        "selector": [
                            "<component_manager>:root/stats/histograms:{MONIKER}",
                        ],
                    },
                ],
                "poll_rate_sec": 1200,
                "project_id": 13,
            }),
        );
        let test_components_config = resource_dir.write(
            "test_components_config.json",
            json!([
                {
                    "id": 3,
                    "label": "Baz",
                    "moniker": "/core/baz",
                }
            ]),
        );
        let test_sampler_project = resource_dir.write(
            "test_project.json",
            json!({
                "metrics": [
                    {
                        "event_codes": [
                            0,
                            0
                        ],
                        "metric_id": 102,
                        "metric_type": "Integer",
                        "selector": "bootstrap/bar/baz:root/samples:some_integer"
                    },
                ],
                "poll_rate_sec": 3,
                "project_id": 5
            }),
        );

        let context = ConfigurationContext {
            feature_set_level: &FeatureSetLevel::Standard,
            build_type: &BuildType::User,
            resource_dir: resource_dir.path(),
            ..ConfigurationContext::default_for_tests()
        };
        let diagnostics = DiagnosticsConfig {
            sampler: SamplerConfig {
                project_configs: vec![test_sampler_project],
                fire: FireConfig {
                    component_configs: vec![test_components_config],
                    project_templates: vec![fire_test_template],
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let mut builder = ConfigurationBuilderImpl::default();
        DiagnosticsSubsystem::define_configuration(
            &context,
            &DiagnosticsSubsystemConfig {
                diagnostics: &diagnostics,
                storage: &StorageConfig::default(),
            },
            &mut builder,
        )
        .expect("defined config");
        let config = builder.build();
        let project_configs: Vec<ProjectConfig> = match config.configuration_capabilities
            ["fuchsia.diagnostics.sampler.ProjectConfigs"]
            .value()
        {
            Value::Array(data) => data
                .into_iter()
                .map(|value| match value {
                    serde_json::Value::String(value) => {
                        serde_json::from_str(&value).expect("valid json")
                    }
                    other => panic!("must be an array of strings, got: {other:?}"),
                })
                .collect(),
            other => panic!("got unexpected project configs: {other:?}"),
        };
        assert_eq!(
            project_configs,
            vec![
                ProjectConfig {
                    project_id: ProjectId(5),
                    customer_id: CustomerId(1),
                    poll_rate_sec: 3000,
                    metrics: vec![
                        MetricConfig {
                            selectors: vec![
                                selectors::parse_verbose(
                                "single_counter:root/samples:counter"
                                    ).unwrap(),
                            ],
                            metric_id: MetricId(104),
                            metric_type: MetricType::Occurrence,
                            event_codes: vec![EventCode(0), EventCode(1)],
                            upload_once: false,
                            project_id: None,
                        }
                    ]
                },
                ProjectConfig {
                    project_id: ProjectId(5),
                    customer_id: CustomerId(1),
                    poll_rate_sec: 3,
                    metrics: vec![
                        MetricConfig {
                            selectors: vec![
                                selectors::parse_verbose(
                                "bootstrap/bar/baz:root/samples:some_integer"
                                    ).unwrap(),
                            ],
                            metric_id: MetricId(102),
                            metric_type: MetricType::Integer,
                            event_codes: vec![EventCode(0), EventCode(0)],
                            upload_once: false,
                            project_id: None,
                        }
                    ]
                },
                ProjectConfig {
                    project_id: ProjectId(13),
                    customer_id: CustomerId(1),
                    poll_rate_sec: 3,
                    metrics: vec![
                        MetricConfig {
                            selectors: vec![
                                selectors::parse_verbose(
                                "component:root/samples:1111111111111111111111111111111111111111111111111111111111111111"
                                    ).unwrap(),
                            ],
                            metric_id: MetricId(3),
                            metric_type: MetricType::Integer,
                            event_codes: vec![EventCode(1)],
                            upload_once: true,
                            project_id: None,
                        },
                        MetricConfig {
                            selectors: vec![
                                selectors::parse_verbose(
                                "component:root/samples:2222222222222222222222222222222222222222222222222222222222222222"
                                    ).unwrap(),
                            ],
                            metric_id: MetricId(3),
                            metric_type: MetricType::Integer,
                            event_codes: vec![EventCode(2)],
                            upload_once: true,
                            project_id: None,
                        }
                    ]
                },
                ProjectConfig {
                    project_id: ProjectId(13),
                    customer_id: CustomerId(1),
                    poll_rate_sec: 1200,
                    metrics: vec![
                        MetricConfig {
                            selectors: vec![
                                selectors::parse_verbose(
                                "<component_manager>:root/stats/histograms:core\\/foo"
                                    ).unwrap(),
                            ],
                            metric_id: MetricId(1),
                            metric_type: MetricType::IntHistogram,
                            event_codes: vec![EventCode(1)],
                            upload_once: false,
                            project_id: None,
                        },
                        MetricConfig {
                            selectors: vec![
                                selectors::parse_verbose(
                                "<component_manager>:root/stats/histograms:core\\/bar"
                                    ).unwrap(),
                            ],
                            metric_id: MetricId(1),
                            metric_type: MetricType::IntHistogram,
                            event_codes: vec![EventCode(2)],
                            upload_once: false,
                            project_id: None,
                        },
                        MetricConfig {
                            selectors: vec![
                                selectors::parse_verbose(
                                "<component_manager>:root/stats/histograms:core\\/baz"
                                    ).unwrap(),
                            ],
                            metric_id: MetricId(1),
                            metric_type: MetricType::IntHistogram,
                            event_codes: vec![EventCode(3)],
                            upload_once: false,
                            project_id: None,
                        },
                    ]
                }
            ]
        );
    }
}
