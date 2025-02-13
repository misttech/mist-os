// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Error};
use glob::{GlobError, Paths};
use log::warn;
use moniker::ExtendedMoniker;
use serde::de::DeserializeOwned;
use std::fs;
use std::path::{Path, PathBuf};

mod common;
pub mod runtime;
// The user facing configuration must not be available in the target device. Only runtime types
// are.
// TODO(https://fxbug.dev/395172409): re-enable
// #[cfg(not(target_os = "fuchsia"))]
pub mod user_facing;
mod utils;

pub use common::*;
use runtime::ProjectConfig;
use user_facing::{ComponentIdInfo, ComponentIdInfoList, ProjectTemplate};

const INSTANCE_IDS_PATH: &str = "config/data/component_id_index";

pub fn load_project_configs(
    sampler_dir: PathBuf,
    fire_dir: Option<PathBuf>,
) -> Result<Vec<ProjectConfig>, Error> {
    let legacy_sampler_config_paths = paths_matching_name(&sampler_dir, "*.json")?;
    let sampler_config_paths = paths_matching_name(&sampler_dir, "*/*.json")?;
    let mut project_configs = load_many(sampler_config_paths)?;
    project_configs.append(&mut load_many(legacy_sampler_config_paths)?);
    if let Some(fire_dir) = fire_dir {
        let fire_project_paths = paths_matching_name(&fire_dir, "*/projects/*.json5")?;
        let fire_component_paths = paths_matching_name(&fire_dir, "*/components.json5")?;
        let fire_project_templates = load_many(fire_project_paths)?;
        let fire_components = load_many::<ComponentIdInfoList>(fire_component_paths)?;
        let mut fire_components =
            fire_components.into_iter().flatten().collect::<Vec<ComponentIdInfo>>();
        match component_id_index::Index::from_fidl_file(INSTANCE_IDS_PATH.into()) {
            Ok(ids) => add_instance_ids(ids, &mut fire_components),
            Err(error) => {
                warn!(
                    error:?;
                    "Unable to read ID file; FIRE selectors with instance IDs won't work"
                );
            }
        };
        let fire_components = ComponentIdInfoList::new(fire_components);
        project_configs.append(&mut expand_fire_projects(fire_project_templates, fire_components)?);
    }
    Ok(project_configs)
}

fn paths_matching_name(path: impl AsRef<Path>, name: &str) -> Result<Paths, Error> {
    let path = path.as_ref();
    let pattern = path.join(name);
    Ok(glob::glob(&pattern.to_string_lossy())?)
}

fn load_many<T: DeserializeOwned>(paths: Paths) -> Result<Vec<T>, Error> {
    paths
        .map(|path: Result<PathBuf, GlobError>| {
            let path = path?;
            let json_string: String =
                fs::read_to_string(&path).with_context(|| format!("parsing {}", path.display()))?;
            let config: T = serde_json5::from_str(&json_string)?;
            Ok(config)
        })
        .collect::<Result<Vec<_>, _>>()
}

fn add_instance_ids(ids: component_id_index::Index, fire_components: &mut Vec<ComponentIdInfo>) {
    for component in fire_components {
        if let ExtendedMoniker::ComponentInstance(moniker) = &component.moniker {
            component.instance_id = ids.id_for_moniker(moniker).cloned();
        }
    }
}
fn expand_fire_projects(
    projects: Vec<ProjectTemplate>,
    components: ComponentIdInfoList,
) -> Result<Vec<ProjectConfig>, Error> {
    projects.into_iter().map(|project| project.expand(&components)).collect::<Result<Vec<_>, _>>()
}
