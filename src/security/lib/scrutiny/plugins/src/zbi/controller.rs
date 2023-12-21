// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::zbi::Zbi,
    anyhow::Result,
    scrutiny::{model::controller::DataController, model::model::DataModel},
    scrutiny_utils::bootfs::BootfsPackageIndex,
    scrutiny_utils::usage::UsageBuilder,
    serde_json::{self, value::Value},
    std::sync::Arc,
};

/// The controller for querying the bootfs files in a product.
#[derive(Default)]
pub struct BootFsFilesController;

impl DataController for BootFsFilesController {
    fn query(&self, model: Arc<DataModel>, _: Value) -> Result<Value> {
        if let Ok(zbi) = model.get::<Zbi>() {
            let mut paths = zbi.bootfs_files.bootfs_files.keys().cloned().collect::<Vec<String>>();
            paths.sort();
            Ok(serde_json::to_value(paths)?)
        } else {
            let empty: Vec<String> = vec![];
            Ok(serde_json::to_value(empty)?)
        }
    }
    fn description(&self) -> String {
        "Returns all the files in bootfs.".to_string()
    }
    fn usage(&self) -> String {
        UsageBuilder::new()
            .name("zbi.bootfs - Lists all the BootFS files found in the ZBI.")
            .summary("zbi.bootfs")
            .description(
                "Lists all the BootFS ZBI files found in the ZBI.\
            More specifically it is looking at the ZBI found in the \
            fuchsia-pkg://fuchsia.com/update package.",
            )
            .build()
    }
}

/// The controller for querying the bootfs packages in a product.
#[derive(Default)]
pub struct BootFsPackagesController;

impl DataController for BootFsPackagesController {
    fn query(&self, model: Arc<DataModel>, _: Value) -> Result<Value> {
        if let Ok(zbi) = model.get::<Zbi>() {
            Ok(serde_json::to_value(zbi.bootfs_packages.clone())?)
        } else {
            Ok(serde_json::to_value(BootfsPackageIndex::default())?)
        }
    }
    fn description(&self) -> String {
        "Returns all the packages in bootfs.".to_string()
    }
    fn usage(&self) -> String {
        UsageBuilder::new()
            .name("zbi.bootfs_packages - Lists all the BootFS packages found in the ZBI.")
            .summary("zbi.bootfs_packages")
            .description(
                "Lists all the BootFS packages found in the ZBI.\
            More specifically it is looking at the ZBI found in the \
            fuchsia-pkg://fuchsia.com/update package.",
            )
            .build()
    }
}

/// The controller for querying the kernel cmdline in a product.
#[derive(Default)]
pub struct ZbiCmdlineController;

impl DataController for ZbiCmdlineController {
    fn query(&self, model: Arc<DataModel>, _: Value) -> Result<Value> {
        if let Ok(zbi) = model.get::<Zbi>() {
            Ok(serde_json::to_value(zbi.cmdline.clone())?)
        } else {
            let empty: Vec<String> = vec![];
            Ok(serde_json::to_value(empty)?)
        }
    }
    fn description(&self) -> String {
        "Returns the zbi cmdline section as a string.".to_string()
    }
    fn usage(&self) -> String {
        UsageBuilder::new()
            .name("zbi.cmdline - Lists the command line params set in the ZBI.")
            .summary("zbi.cmdline")
            .description(
                "Lists all the command line parameters set in the ZBI. \
            More specifically it is looking at the ZBI found in the \
            fuchsia-pkg://fuchsia.com/update package.",
            )
            .build()
    }
}

#[derive(Default)]
pub struct ZbiSectionsController {}

impl DataController for ZbiSectionsController {
    fn query(&self, model: Arc<DataModel>, _: Value) -> Result<Value> {
        if let Ok(zbi) = model.get::<Zbi>() {
            let mut sections = vec![];
            for section in zbi.sections.iter() {
                sections.push(section.section_type.clone());
            }
            Ok(serde_json::to_value(sections)?)
        } else {
            let empty: Vec<String> = vec![];
            Ok(serde_json::to_value(empty)?)
        }
    }
    fn description(&self) -> String {
        "Returns all the typed sections found in the zbi.".to_string()
    }
    fn usage(&self) -> String {
        UsageBuilder::new()
            .name("zbi.sections - Lists the section types set in the ZBI.")
            .summary("zbi.sections")
            .description(
                "Lists all the unique section types set in the ZBI. \
            More specifically it is looking at the ZBI found in the \
            fuchsia-pkg://fuchsia.com/update package.",
            )
            .build()
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*, scrutiny_testing::fake::*, scrutiny_utils::bootfs::BootfsFileIndex,
        serde_json::json, std::collections::HashSet,
    };

    #[test]
    fn bootfs_returns_files() {
        let model = fake_data_model();
        let mut zbi = Zbi {
            deps: HashSet::default(),
            sections: vec![],
            bootfs_files: BootfsFileIndex::default(),
            bootfs_packages: BootfsPackageIndex::default(),
            cmdline: vec![],
        };
        zbi.bootfs_files.bootfs_files.insert("foo".to_string(), vec![]);
        model.set(zbi).unwrap();
        let controller = BootFsFilesController::default();
        let bootfs: Vec<String> =
            serde_json::from_value(controller.query(model, json!("")).unwrap()).unwrap();
        assert_eq!(bootfs, vec!["foo".to_string()]);
    }

    #[test]
    fn zbi_cmdline() {
        let model = fake_data_model();
        let zbi = Zbi {
            deps: HashSet::default(),
            sections: vec![],
            bootfs_files: BootfsFileIndex::default(),
            bootfs_packages: BootfsPackageIndex::default(),
            cmdline: vec!["foo".to_string()],
        };
        model.set(zbi).unwrap();
        let controller = ZbiCmdlineController::default();
        let cmdline: Vec<String> =
            serde_json::from_value(controller.query(model, json!("")).unwrap()).unwrap();
        assert_eq!(cmdline, vec!["foo".to_string()]);
    }
}
