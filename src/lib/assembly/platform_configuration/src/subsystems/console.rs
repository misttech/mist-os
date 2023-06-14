// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::BTreeSet;

use fidl_fuchsia_logger::MAX_TAGS;

use crate::subsystems::prelude::*;

const BASE_CONSOLE_ALLOWED_TAGS: &[&str] = &[];
static_assertions::const_assert!(BASE_CONSOLE_ALLOWED_TAGS.len() <= MAX_TAGS as usize);

const BASE_CONSOLE_DENIED_TAGS: &[&str] = &[];

pub(crate) struct ConsoleSubsystemConfig;
impl DefineSubsystemConfiguration<Vec<String>> for ConsoleSubsystemConfig {
    fn define_configuration(
        _context: &ConfigurationContext<'_>,
        additional_serial_log_tags: &Vec<String>,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // Configure the serial console.
        let allowed_log_tags = {
            let mut allowed_log_tags: BTreeSet<_> =
                BASE_CONSOLE_ALLOWED_TAGS.iter().map(|s| s.to_string()).collect();

            let num_product_tags = additional_serial_log_tags.len();
            let max_product_tags = MAX_TAGS as usize - BASE_CONSOLE_ALLOWED_TAGS.len();
            if num_product_tags > max_product_tags {
                return Err(anyhow::anyhow!(
                    "Max {} tags can be forwarded to serial, got {:?}",
                    max_product_tags,
                    additional_serial_log_tags,
                ));
            }
            allowed_log_tags.extend(additional_serial_log_tags.iter().cloned());
            allowed_log_tags.into_iter().collect::<Vec<_>>()
        };
        let denied_log_tags: Vec<_> =
            BASE_CONSOLE_DENIED_TAGS.iter().map(|s| s.to_string()).collect();

        builder
            .package("console")
            .component("meta/console.cm")?
            .field("allowed_log_tags", allowed_log_tags)?
            .field("denied_log_tags", denied_log_tags)?;

        Ok(())
    }
}
