// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use errors::ffx_bail;
use ffx_scrutiny_verify_args::routes::{default_capability_types, Command};
use scrutiny_config::{ConfigBuilder, ModelConfig};
use scrutiny_frontend::{command_builder::CommandBuilder, launcher};
use scrutiny_plugins::verify::{CapabilityRouteResults, ResultsForCapabilityType};
use std::{collections::HashSet, path::PathBuf};

struct Query {
    capability_types: Vec<String>,
    response_level: String,
    product_bundle: PathBuf,
    component_tree_config_path: Option<PathBuf>,
    tmp_dir_path: Option<PathBuf>,
}

impl Query {
    fn with_temporary_directory(mut self, tmp_dir_path: Option<&PathBuf>) -> Self {
        self.tmp_dir_path = tmp_dir_path.map(PathBuf::clone);
        self
    }
}

impl From<&Command> for Query {
    fn from(cmd: &Command) -> Self {
        // argh(default = "vec![...]") does not work due to failed trait bound:
        // FromStr on Vec<_>. Apply default when vec is empty.
        let capability_types = if cmd.capability_type.len() > 0 {
            cmd.capability_type.clone()
        } else {
            default_capability_types()
        }
        .into_iter()
        .map(|capability_type| capability_type.into())
        .collect();
        Query {
            capability_types,
            response_level: cmd.response_level.clone().into(),
            product_bundle: cmd.product_bundle.clone(),
            component_tree_config_path: cmd.component_tree_config.clone(),
            tmp_dir_path: None,
        }
    }
}

pub async fn verify(
    cmd: &Command,
    tmp_dir: Option<&PathBuf>,
    recovery: bool,
) -> Result<HashSet<PathBuf>> {
    let query: Query = Query::from(cmd).with_temporary_directory(tmp_dir);
    let model = if recovery {
        ModelConfig::from_product_bundle_recovery(&query.product_bundle)
    } else {
        ModelConfig::from_product_bundle(&query.product_bundle)
    }?;
    let command = CommandBuilder::new("verify.capability_routes")
        .param("capability_types", query.capability_types.join(" "))
        .param("response_level", &query.response_level)
        .build();
    let plugins = vec![
        "ZbiPlugin",
        "AdditionalBootConfigPlugin",
        "StaticPkgsPlugin",
        "CorePlugin",
        "VerifyPlugin",
    ];
    let mut config = ConfigBuilder::with_model(model).command(command).plugins(plugins).build();
    config.runtime.model.component_tree_config_path = query.component_tree_config_path;
    config.runtime.model.tmp_dir_path = query.tmp_dir_path;
    config.runtime.logging.silent_mode = true;

    let results = launcher::launch_from_config(config).context("Failed to launch scrutiny")?;
    let mut route_analysis: CapabilityRouteResults = serde_json::from_str(&results)
        .context(format!("Failed to deserialize verify routes results: {}", results))?;

    // Human-readable messages associated with errors and warnings drawn from `route_analysis`.
    let mut human_readable_errors = vec![];

    // Human-readable messages associated with info drawn from `route_analysis`.
    let mut human_readable_messages = vec![];

    // Populate human-readable collections with content from `route_analysis.results`.
    let mut ok_analysis = vec![];
    for entry in route_analysis.results.iter_mut() {
        // If there are any errors, produce the human-readable version of each.
        for error in entry.results.errors.iter_mut() {
            // Remove all route segments so they don't show up in JSON snippet.
            let mut context: Vec<String> = error
                .route
                .drain(..)
                .enumerate()
                .map(|(i, s)| {
                    let step = format!("step {}", i + 1);
                    format!("{:>8}: {}", step, s)
                })
                .collect();

            // Add the failure to the route segments.
            let error = format!(
                "❌ ERROR: {}\n    Moniker: {}\n    Capability: {}",
                error.error.message,
                error.using_node,
                error.capability.as_ref().map(|c| c.to_string()).unwrap_or("".into())
            );
            context.push(error);

            // The context must begin from the point of failure.
            context.reverse();

            // Chain the error context into a single string.
            let error_with_context = context.join("\n");
            human_readable_errors.push(error_with_context);
        }

        for warning in entry.results.warnings.iter_mut() {
            // Remove all route segments so they don't show up in JSON snippet.
            let mut context: Vec<String> = warning
                .route
                .drain(..)
                .enumerate()
                .map(|(i, s)| {
                    let step = format!("step {}", i + 1);
                    format!("{:>8}: {}", step, s)
                })
                .collect();

            // Add the warning to the route segments.
            let warning = format!(
                "⚠️ WARNING: {}\n    Moniker: {}\n    Capability: {}",
                warning.warning.message,
                warning.using_node,
                warning.capability.as_ref().map(|c| c.to_string()).unwrap_or("".into())
            );
            context.push(warning);

            // The context must begin from the warning message.
            context.reverse();

            // Chain the warning context into a single string.
            let warning_with_context = context.join("\n");
            human_readable_errors.push(warning_with_context);
        }

        let mut ok_item = ResultsForCapabilityType {
            capability_type: entry.capability_type.clone(),
            results: Default::default(),
        };
        for ok in entry.results.ok.iter_mut() {
            let mut context: Vec<String> = if &query.response_level != "verbose" {
                // Remove all route segments so they don't show up in JSON snippet.
                ok.route
                    .drain(..)
                    .enumerate()
                    .map(|(i, s)| {
                        let step = format!("step {}", i + 1);
                        format!("{:>8}: {}", step, s)
                    })
                    .collect()
            } else {
                vec![]
            };
            context.push(format!("ℹ️ INFO: {}: {}", ok.using_node, ok.capability));

            // The context must begin from the capability description.
            context.reverse();

            // Chain the report context into a single string.
            let message_with_context = context.join("\n");
            human_readable_messages.push(message_with_context);

            // Accumulate ok data outside the collection reported on error/warning.
            ok_item.results.ok.push(ok.clone());
        }
        // Remove ok data from collection reported on error/warning, and store extracted `ok_item`.
        entry.results.ok = vec![];
        ok_analysis.push(ok_item);
    }

    // Report human-readable info without bailing.
    if !human_readable_messages.is_empty() {
        println!(
            "
Static Capability Flow Analysis Info:
The route verifier is reporting all capability routes in this build.

>>>>>> START OF JSON SNIPPET
{}
<<<<<< END OF JSON SNIPPET

Route messages:
{}",
            serde_json::to_string_pretty(&ok_analysis).unwrap(),
            human_readable_messages.join("\n\n")
        );
    }

    // Bail after reporting human-readable error/warning messages.
    if !human_readable_errors.is_empty() {
        ffx_bail!(
            "
Static Capability Flow Analysis Error:
The route verifier failed to verify all capability routes in this build.

See https://fuchsia.dev/go/components/static-analysis-errors

>>>>>> START OF JSON SNIPPET
{}
<<<<<< END OF JSON SNIPPET

Please fix the following errors:
{}",
            serde_json::to_string_pretty(&route_analysis.results).unwrap(),
            human_readable_errors.join("\n\n")
        );
    }

    Ok(route_analysis.deps)
}
