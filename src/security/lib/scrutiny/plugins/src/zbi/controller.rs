// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::zbi::Zbi;
use anyhow::Result;
use scrutiny::model::controller::DataController;
use scrutiny::model::model::DataModel;
use scrutiny_utils::usage::UsageBuilder;
use serde_json::value::Value;
use std::sync::Arc;

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
