// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A tool to:
//! - lookup product bundle description information to find the transfer URL.

use anyhow::Result;
use ffx_core::ffx_plugin;
use ffx_product_list::{pb_list_impl, ProductBundle};
use ffx_product_lookup_args::LookupCommand;
use ffx_writer::Writer;
use pbms::AuthFlowChoice;
use std::io::{stderr, stdin, stdout, Write};

/// `ffx product lookup` sub-command.
#[ffx_plugin()]
pub async fn pb_lookup(
    cmd: LookupCommand,
    #[ffx(machine = ProductBundle)] mut writer: Writer,
) -> Result<()> {
    let mut input = stdin();
    // Emit machine progress info to stderr so users can redirect it to /dev/null.
    let mut output = if writer.is_machine() {
        Box::new(stderr()) as Box<dyn Write + Send + Sync>
    } else {
        Box::new(stdout())
    };
    let mut err_out = stderr();
    let ui = structured_ui::TextUi::new(&mut input, &mut output, &mut err_out);
    let product = pb_lookup_impl(&cmd.auth, cmd.base_url, &cmd.name, &cmd.version, &ui).await?;
    // Don't emit progress information if we're writing a machine
    // representation.
    if writer.is_machine() {
        writer.machine(&product)?;
    } else {
        writeln!(writer, "{}", product.transfer_manifest_url)?;
    }
    Ok(())
}

pub async fn pb_lookup_impl<I>(
    auth: &AuthFlowChoice,
    override_base_url: Option<String>,
    name: &str,
    version: &str,
    ui: &I,
) -> Result<ProductBundle>
where
    I: structured_ui::Interface,
{
    let start = std::time::Instant::now();
    tracing::info!("---------------------- Lookup Begin ----------------------------");

    let products =
        pb_list_impl(auth, override_base_url, Some(version.to_string()), None, ui).await?;

    tracing::debug!("Looking for product bundle {}, version {}", name, version);
    let mut products = products
        .iter()
        .filter(|x| x.name == name)
        .filter(|x| x.product_version == version)
        .map(|x| x.to_owned());

    let Some(product) = products.next() else {
        tracing::debug!("products {:?}", products);
        eprintln!("Error: No product matching name {}, version {} found.", name, version);
        std::process::exit(1);
    };

    if products.next().is_some() {
        tracing::debug!("products {:?}", products);
        eprintln!(
            "More than one matching product found. The base-url may have poorly formed data."
        );
        std::process::exit(1);
    }

    tracing::debug!("Total ffx product lookup runtime {} seconds.", start.elapsed().as_secs_f32());
    tracing::debug!("End");

    Ok(product)
}

#[cfg(test)]
mod test {
    use {super::*, ffx_writer::Format, temp_test_env::TempTestEnv};

    const PB_MANIFEST_NAME: &'static str = "product_bundles.json";

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_pb_lookup_impl() {
        let test_env = TempTestEnv::new().expect("test_env");
        let mut f =
            std::fs::File::create(test_env.home.join(PB_MANIFEST_NAME)).expect("file create");
        f.write_all(
            r#"[{
            "name": "fake_name",
            "product_version": "fake_version",
            "transfer_manifest_url": "fake_url"
            }]"#
            .as_bytes(),
        )
        .expect("write_all");

        let ui = structured_ui::MockUi::new();
        let product = pb_lookup_impl(
            &AuthFlowChoice::Default,
            Some(format!("file:{}", test_env.home.display())),
            "fake_name",
            "fake_version",
            &ui,
        )
        .await
        .expect("testing lookup");

        assert_eq!(
            product,
            ProductBundle {
                name: "fake_name".into(),
                product_version: "fake_version".into(),
                transfer_manifest_url: "fake_url".into(),
            },
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_bp_lookup_machine_mode() {
        let test_env = TempTestEnv::new().expect("test_env");
        let mut f =
            std::fs::File::create(test_env.home.join(PB_MANIFEST_NAME)).expect("file create");
        f.write_all(
            r#"[{
            "name": "fake_name",
            "product_version": "fake_version",
            "transfer_manifest_url": "fake_url"
            }]"#
            .as_bytes(),
        )
        .expect("write_all");

        let writer = Writer::new_test(Some(Format::Json));
        let () = pb_lookup(
            LookupCommand {
                auth: AuthFlowChoice::Default,
                base_url: Some(format!("file:{}", test_env.home.display())),
                name: "fake_name".into(),
                version: "fake_version".into(),
            },
            writer.clone(),
        )
        .await
        .expect("testing lookup");

        let product: ProductBundle = serde_json::from_str(&writer.test_output().unwrap()).unwrap();
        assert_eq!(
            product,
            ProductBundle {
                name: "fake_name".into(),
                product_version: "fake_version".into(),
                transfer_manifest_url: "fake_url".into(),
            },
        );
    }
}
