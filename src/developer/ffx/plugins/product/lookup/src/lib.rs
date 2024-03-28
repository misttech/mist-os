// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A tool to:
//! - lookup product bundle description information to find the transfer URL.

use ffx_product::{CommandStatus, MachineOutput, MachineUi};
use ffx_product_list::{pb_list_impl, ProductBundle};
use ffx_product_lookup_args::LookupCommand;
use fho::{bug, return_user_error, FfxMain, FfxTool, Result, ToolIO, VerifiedMachineWriter};
use pbms::AuthFlowChoice;
use std::io::{stdin, stdout, Write};

#[derive(FfxTool)]
pub struct PbLookupTool {
    #[command]
    cmd: LookupCommand,
}

fho::embedded_plugin!(PbLookupTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for PbLookupTool {
    type Writer = VerifiedMachineWriter<MachineOutput<ProductBundle>>;
    async fn main(self, writer: Self::Writer) -> Result<()> {
        if writer.is_machine() {
            self.do_machine_main(writer).await
        } else {
            self.do_text_main(writer).await
        }
    }
}

impl PbLookupTool {
    async fn do_machine_main(&self, writer: <PbLookupTool as fho::FfxMain>::Writer) -> Result<()> {
        let ui = MachineUi::new(writer);

        match pb_lookup_impl(
            &self.cmd.auth,
            &self.cmd.base_url,
            &self.cmd.name,
            &self.cmd.version,
            &ui,
        )
        .await
        {
            Ok(product_bundle) => {
                ui.machine(MachineOutput::Data(product_bundle))?;
                return Ok(());
            }

            Err(e) => {
                ui.machine(MachineOutput::CommandStatus(CommandStatus::UnexpectedError {
                    message: e.to_string(),
                }))?;
                return Err(e.into());
            }
        }
    }

    async fn do_text_main(&self, mut writer: <PbLookupTool as fho::FfxMain>::Writer) -> Result<()> {
        let mut output = stdout();
        let mut err_out = writer.stderr();
        let mut input = stdin();
        let ui = structured_ui::TextUi::new(&mut input, &mut output, &mut err_out);
        let product = pb_lookup_impl(
            &self.cmd.auth,
            &self.cmd.base_url,
            &self.cmd.name,
            &self.cmd.version,
            &ui,
        )
        .await?;

        writeln!(writer, "{}", product.transfer_manifest_url).map_err(|e| bug!("{e}"))?;
        Ok(())
    }
}

pub async fn pb_lookup_impl<I>(
    auth: &AuthFlowChoice,
    override_base_url: &Option<String>,
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
        pb_list_impl(auth, override_base_url.clone(), Some(version.to_string()), None, ui).await?;

    tracing::debug!("Looking for product bundle {}, version {}", name, version);
    let mut products = products
        .iter()
        .filter(|x| x.name == name)
        .filter(|x| x.product_version == version)
        .map(|x| x.to_owned());

    let Some(product) = products.next() else {
        tracing::debug!("products {:?}", products);
        return_user_error!("Error: No product matching name {}, version {} found.", name, version);
    };

    if products.next().is_some() {
        tracing::debug!("products {:?}", products);
        return_user_error!(
            "More than one matching product found. The base-url may have poorly formed data."
        );
    }

    tracing::debug!("Total ffx product lookup runtime {} seconds.", start.elapsed().as_secs_f32());
    tracing::debug!("End");

    Ok(product)
}

#[cfg(test)]
mod test {
    use {
        super::*,
        fho::{Format, TestBuffers},
        temp_test_env::TempTestEnv,
    };

    const PB_MANIFEST_NAME: &'static str = "product_bundles.json";

    #[fuchsia::test]
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
            &Some(format!("file:{}", test_env.home.display())),
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

    #[fuchsia::test]
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

        let buffers = TestBuffers::default();
        let writer = VerifiedMachineWriter::new_test(Some(Format::Json), &buffers);
        let tool = PbLookupTool {
            cmd: LookupCommand {
                auth: AuthFlowChoice::Default,
                base_url: Some(format!("file:{}", test_env.home.display())),
                name: "fake_name".into(),
                version: "fake_version".into(),
            },
        };

        tool.main(writer).await.expect("testing lookup");

        let expected = serde_json::to_string(&MachineOutput::Data(ProductBundle {
            name: "fake_name".into(),
            product_version: "fake_version".into(),
            transfer_manifest_url: "fake_url".into(),
        }))
        .expect("serialize data");
        assert_eq!(buffers.into_stdout_str(), format!("{expected}\n"));
    }
}
