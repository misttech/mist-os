// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use ffx_config::EnvironmentContext;
use fidl_fuchsia_tracing_controller::{ProviderSpec, TraceConfig};
use regex::Regex;
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

pub async fn expand_categories(
    context: &EnvironmentContext,
    categories: Vec<String>,
) -> Result<Vec<String>> {
    let mut expanded_categories = BTreeSet::new();
    for category in categories {
        match category.strip_prefix('#') {
            Some(category_group_name) => {
                let category_group = get_category_group(context, category_group_name).await?;
                expanded_categories.extend(category_group);
            }
            None => {
                validate_category_name(&category)?;
                expanded_categories.insert(category);
            }
        }
    }
    Ok(expanded_categories.into_iter().collect())
}

pub async fn get_category_group(
    ctx: &EnvironmentContext,
    category_group_name: &str,
) -> Result<Vec<String>> {
    let category_group = ctx
        .get::<Vec<String>, _>(&format!("trace.category_groups.{}", category_group_name))
        .context(format!(
            "Error: no category group found for {0}, you can add this category locally by calling \
              `ffx config set trace.category_groups.{0} '[\"list\", \"of\", \"categories\"]'`\
              or globally by adding it to data/config.json in the ffx trace plugin.",
            category_group_name
        ))?;
    for category in &category_group {
        validate_category_name(&category).context(format!(
            "Error: #{} contains an invalid category \"{}\"",
            category_group_name, category
        ))?;
    }
    Ok(category_group)
}

pub async fn get_category_group_names(ctx: &EnvironmentContext) -> Result<Vec<String>> {
    let all_groups = ctx
        .query("trace.category_groups")
        .select(ffx_config::SelectMode::All)
        .get::<Value>()
        .context("could not query `trace.category_groups` in config.")?;
    let mut group_names: Vec<String> = all_groups
        .as_array()
        .unwrap()
        .into_iter()
        .flat_map(|subgroups| subgroups.as_object().unwrap())
        .map(|(group_name, _)| group_name)
        .cloned()
        .collect();
    group_names.sort_unstable();
    Ok(group_names)
}

pub fn validate_category_name(category_name: &str) -> Result<()> {
    static VALID_CATEGORY_REGEX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"^[^\*,\s]*\*?$"#).unwrap());
    if !VALID_CATEGORY_REGEX.is_match(category_name) {
        return Err(anyhow!("Error: category \"{}\" is invalid", category_name));
    }
    Ok(())
}

pub fn map_categories_to_providers(categories: &Vec<String>) -> TraceConfig {
    let mut provider_specific_categories = HashMap::<&str, Vec<String>>::new();
    let mut umbrella_categories = vec![];
    for category in categories {
        if let Some((provider_name, category)) = category.split_once("/") {
            provider_specific_categories
                .entry(provider_name)
                .and_modify(|categories| categories.push(category.to_string()))
                .or_insert_with(|| vec![category.to_string()]);
        } else {
            umbrella_categories.push(category.clone());
        }
    }

    let mut trace_config = TraceConfig::default();
    if !categories.is_empty() {
        trace_config.categories = Some(umbrella_categories.clone());
    }
    if !provider_specific_categories.is_empty() {
        trace_config.provider_specs = Some(
            provider_specific_categories
                .into_iter()
                .map(|(name, categories)| ProviderSpec {
                    name: Some(name.to_string()),
                    categories: Some(categories),
                    ..Default::default()
                })
                .collect(),
        );
    }
    trace_config
}

pub fn ir_files_list(env_ctx: &EnvironmentContext) -> Option<Vec<String>> {
    let mut ir_files = Vec::new();
    #[allow(clippy::or_fun_call)] // TODO(https://fxbug.dev/379717780)
    let build_dir = env_ctx.build_dir().unwrap_or(&std::path::Path::new(""));
    let all_fidl_json_path = build_dir.join("all_fidl_json.txt");
    match std::fs::read_to_string(all_fidl_json_path) {
        Ok(file_list) => {
            for line in file_list.lines() {
                if let Some(ir_file_path) = build_dir.join(line).to_str() {
                    ir_files.push(ir_file_path.to_string());
                }
            }
        }
        Err(_) => return None,
    };
    Some(ir_files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    async fn test_get_category_group() {
        let env = ffx_config::test_init().await.unwrap();
        let birds = vec!["chickens", "bald_eagle", "blue-jay", "hawk*", "goose:gosling"];
        env.context
            .query("trace.category_groups.birds")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(birds))
            .await
            .unwrap();
        assert_eq!(birds, get_category_group(&env.context, "birds").await.unwrap());
    }

    #[fuchsia::test]
    async fn test_get_category_group_names() {
        let env = ffx_config::test_init().await.unwrap();
        let birds = vec!["chickens", "ducks"];
        let bees = vec!["honey", "bumble"];
        env.context
            .query("trace.category_groups.birds")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(birds))
            .await
            .unwrap();
        env.context
            .query("trace.category_groups.bees")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(bees))
            .await
            .unwrap();
        env.context
            .query("trace.category_groups.*invalid")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(bees))
            .await
            .unwrap();
        assert!(get_category_group_names(&env.context)
            .await
            .unwrap()
            .contains(&"birds".to_owned()));
        assert!(get_category_group_names(&env.context).await.unwrap().contains(&"bees".to_owned()));
        assert!(get_category_group_names(&env.context)
            .await
            .unwrap()
            .contains(&"*invalid".to_owned()));
    }

    #[fuchsia::test]
    async fn test_get_category_group_not_found() {
        let env = ffx_config::test_init().await.unwrap();
        let err = get_category_group(&env.context, "not_found").await.unwrap_err();
        assert!(
            err.to_string().contains("Error: no category group found for not_found"),
            "the actual value was \"{}\"",
            err.to_string()
        );
    }

    const INVALID_CATEGORIES: &[&str] =
        &["chic*kens", "*turkeys", "golden eagle", "ha,wk*", "goose:gosl\"ing"];

    #[fuchsia::test]
    async fn test_get_category_group_invalid_category() {
        let env = ffx_config::test_init().await.unwrap();
        for invalid_category in INVALID_CATEGORIES {
            env.context
                .query("trace.category_groups.flawed")
                .level(Some(ffx_config::ConfigLevel::User))
                .set(json!(vec![invalid_category]))
                .await
                .unwrap();
            let err = get_category_group(&env.context, "flawed").await.unwrap_err();
            let expected_message = format!("invalid category \"{}\"", invalid_category);
            assert!(
                err.to_string().contains(&expected_message),
                "the actual value was \"{}\"",
                err.to_string()
            );
        }
    }

    #[fuchsia::test]
    async fn test_expand_categories() {
        let env = ffx_config::test_init().await.unwrap();
        let birds = vec!["chickens", "bald_eagle", "hawk*", "goose:gosling", "blue-jay"];
        env.context
            .query("trace.category_groups.birds")
            .level(Some(ffx_config::ConfigLevel::User))
            .set(json!(birds))
            .await
            .unwrap();
        // The result should have all groups expanded, merge duplicate categories, and sort them.
        assert_eq!(
            vec!["*", "bald_eagle", "blue-jay", "chickens", "dove*", "goose:gosling", "hawk*"],
            expand_categories(
                &env.context,
                vec![
                    "dove*".to_string(),
                    "bald_eagle".to_string(),
                    "#birds".to_string(),
                    "*".to_string()
                ]
            )
            .await
            .unwrap()
        );
    }

    #[fuchsia::test]
    async fn test_expand_categories_invalid() {
        let env = ffx_config::test_init().await.unwrap();
        for invalid_category in INVALID_CATEGORIES {
            let err = expand_categories(&env.context, vec![invalid_category.to_string()])
                .await
                .unwrap_err();
            let expected_message = format!("category \"{}\" is invalid", invalid_category);
            assert!(
                err.to_string().contains(&expected_message),
                "the actual value was \"{}\"",
                err.to_string()
            );
        }
    }

    #[fuchsia::test]
    async fn test_curated_category_groups_valid() {
        let env = ffx_config::test_init().await.unwrap();

        // Get all of the category groups found in config.json
        let category_groups_json: serde_json::Value =
            env.context.get("trace.category_groups").unwrap();

        for category_group_name in category_groups_json.as_object().unwrap().keys() {
            let category_group =
                get_category_group(&env.context, category_group_name).await.unwrap();
            assert_ne!(0, category_group.len());
        }
    }

    #[test]
    fn test_map_categories_to_providers() {
        let expected_trace_config = TraceConfig {
            categories: Some(vec!["talon".to_string(), "beak".to_string()]),
            provider_specs: Some(vec![
                ProviderSpec {
                    name: Some("falcon".to_string()),
                    categories: Some(vec!["prairie".to_string(), "peregrine".to_string()]),
                    ..Default::default()
                },
                ProviderSpec {
                    name: Some("owl".to_string()),
                    categories: Some(vec![
                        "screech".to_string(),
                        "elf".to_string(),
                        "snowy".to_string(),
                    ]),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };

        let mut actual_trace_config = map_categories_to_providers(&vec![
            "owl/screech".to_string(),
            "owl/elf".to_string(),
            "owl/snowy".to_string(),
            "falcon/prairie".to_string(),
            "talon".to_string(),
            "beak".to_string(),
            "falcon/peregrine".to_string(),
        ]);

        // Lexicographically sort the provider specs on names to ensure a stable test.
        // The order doesn't matter, but it can vary with different platforms and compiler flags.
        actual_trace_config
            .provider_specs
            .as_mut()
            .unwrap()
            .sort_unstable_by_key(|s| s.name.clone().unwrap());
        assert_eq!(expected_trace_config, actual_trace_config);
    }
}
