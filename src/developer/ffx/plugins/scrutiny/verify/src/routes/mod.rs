// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod allowlist;

use allowlist::{AllowlistFilter, UnversionedAllowlist, V0Allowlist, V1Allowlist};
use anyhow::{Context, Result};
use errors::ffx_bail;
use ffx_scrutiny_verify_args::routes::{default_capability_types, Command};
use scrutiny_config::{ConfigBuilder, ModelConfig};
use scrutiny_frontend::{command_builder::CommandBuilder, launcher};
use scrutiny_plugins::verify::{CapabilityRouteResults, ResultsForCapabilityType};
use serde_json;
use std::{collections::HashSet, fs, io::Read, path::PathBuf};

struct Query {
    capability_types: Vec<String>,
    response_level: String,
    product_bundle: PathBuf,
    allowlist_paths: Vec<PathBuf>,
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
            allowlist_paths: cmd.allowlist.clone(),
            component_tree_config_path: cmd.component_tree_config.clone(),
            tmp_dir_path: None,
        }
    }
}

fn load_allowlist(allowlist_paths: &Vec<PathBuf>) -> Result<Box<dyn AllowlistFilter>> {
    let builders = vec![UnversionedAllowlist::new(), V0Allowlist::new(), V1Allowlist::new()];
    let mut err = None;

    for mut builder in builders.into_iter() {
        for path in allowlist_paths.iter() {
            let reader: Box<dyn Read> =
                Box::new(fs::File::open(path).context("Failed to open allowlist fragment")?);
            if let Err(load_err) = builder.load(reader) {
                err = Some(load_err);
                break;
            }
        }
        if err.is_none() {
            return Ok(builder.build());
        }
    }

    Err(err.unwrap())
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
    let plugins =
        vec!["AdditionalBootConfigPlugin", "StaticPkgsPlugin", "CorePlugin", "VerifyPlugin"];
    let mut config = ConfigBuilder::with_model(model).command(command).plugins(plugins).build();
    config.runtime.model.component_tree_config_path = query.component_tree_config_path;
    config.runtime.model.tmp_dir_path = query.tmp_dir_path;
    config.runtime.logging.silent_mode = true;

    let results = launcher::launch_from_config(config).context("Failed to launch scrutiny")?;
    let route_analysis: CapabilityRouteResults = serde_json::from_str(&results)
        .context(format!("Failed to deserialize verify routes results: {}", results))?;

    let allowlist_filter = load_allowlist(&query.allowlist_paths)
        .context("Failed to parse all allowlist fragments from supported format")?;

    // Capability type-bucketed errors, warnings, and/or info.
    let mut filtered_analysis = allowlist_filter.filter_analysis(route_analysis.results);

    // Human-readable messages associated with errors and warnings drawn from `filtered_analysis`.
    let mut human_readable_errors = vec![];

    // Human-readable messages associated with info drawn from `filtered_analysis`.
    let mut human_readable_messages = vec![];

    // Clean `filtered_analysis.results` to contain only error and warning data relevant to
    // allowlist entries, and populate human-readable collections.
    let mut ok_analysis = vec![];
    for entry in filtered_analysis.iter_mut() {
        // If there are any errors, produce the human-readable version of each.
        for error in entry.results.errors.iter_mut() {
            // Remove all route segments so they don't show up in allowlist JSON snippet.
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
                error.error.message, error.using_node, error.capability
            );
            context.push(error);

            // The context must begin from the point of failure.
            context.reverse();

            // Chain the error context into a single string.
            let error_with_context = context.join("\n");
            human_readable_errors.push(error_with_context);
        }

        for warning in entry.results.warnings.iter_mut() {
            // Remove all route segments so they don't show up in allowlist JSON snippet.
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
                warning.warning.message, warning.using_node, warning.capability
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

If the broken route is required for a transition it can be temporarily added
to the JSON allowlist located at: {:?}

>>>>>> START OF JSON SNIPPET
{}
<<<<<< END OF JSON SNIPPET

Alternatively, attempt to fix the following errors:
{}",
            query.allowlist_paths,
            serde_json::to_string_pretty(&filtered_analysis).unwrap(),
            human_readable_errors.join("\n\n")
        );
    }

    Ok(route_analysis.deps)
}
