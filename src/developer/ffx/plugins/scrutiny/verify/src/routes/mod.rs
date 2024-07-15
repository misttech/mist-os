// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use cm_rust::CapabilityTypeName;
use errors::ffx_bail;
use ffx_scrutiny_verify_args::routes::Command;
use scrutiny_frontend::scrutiny2::Scrutiny;
use scrutiny_plugins::verify::controller::capability_routing::ResponseLevel;
use scrutiny_plugins::verify::ResultsForCapabilityType;
use std::collections::HashSet;
use std::path::PathBuf;

pub async fn verify(
    cmd: &Command,
    _tmp_dir: Option<&PathBuf>,
    recovery: bool,
) -> Result<HashSet<PathBuf>> {
    let capability_types = if cmd.capability_type.len() > 0 {
        HashSet::from_iter(cmd.capability_type.iter().cloned())
    } else {
        HashSet::from([CapabilityTypeName::Directory, CapabilityTypeName::Protocol])
    };

    let mut scrutiny = if recovery {
        Scrutiny::from_product_bundle_recovery(&cmd.product_bundle)
    } else {
        Scrutiny::from_product_bundle(&cmd.product_bundle)
    }?;
    if let Some(config) = &cmd.component_tree_config {
        scrutiny.set_component_tree_config_path(config);
    }
    let artifacts = scrutiny.collect()?;
    let mut route_analysis =
        artifacts.get_capability_route_results(capability_types, &cmd.response_level)?;

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
            let mut context: Vec<String> = if cmd.response_level != ResponseLevel::Verbose {
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
