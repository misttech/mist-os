// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Use this macro to use the new subtool interfaces in a plugin embedded in ffx (or
/// another subtool). It takes the type that implements `FfxTool` and `FfxMain` as an
/// argument and sets up the global functions that the old `#[plugin()]` interface
/// used to do.
#[macro_export]
macro_rules! embedded_plugin {
    ($tool:ty) => {
        pub async fn ffx_plugin_impl(
            injector: &Option<::std::sync::Arc<dyn ffx_core::Injector>>,
            cmd: <$tool as $crate::FfxTool>::Command,
        ) -> $crate::Result<()> {
            #[allow(unused_imports)]
            use $crate::macro_deps::{
                argh, bug, ffx_writer::Format, global_env_context, return_bug, FfxCommandLine,
            };
            use $crate::FfxMain as _;

            let ffx = FfxCommandLine::from_env()?;
            let context = if let Some(gc) = global_env_context() {
                gc
            } else {
                $crate::macro_deps::return_bug!("global env context unavailable")
            };
            let injector = injector.clone();
            #[allow(deprecated)] // injector field.
            let env = $crate::FhoEnvironment {
                behavior: $crate::connection_behavior(&ffx, &injector, &context).await?,
                ffx,
                context,
                injector,
                lookup: std::sync::Arc::new($crate::DeviceLookupDefaultImpl),
            };

            // Create the writer, and if the schema is flag is set,
            // try to print it and then exit.
            let mut writer: <$tool as $crate::FfxMain>::Writer =
                $crate::TryFromEnv::try_from_env(&env).await?;
            if env.ffx.global.schema {
                use $crate::macro_deps::ffx_writer::ToolIO;
                if <<$tool as $crate::FfxMain>::Writer as $crate::ToolIO>::has_schema() {
                    writer.try_print_schema()?;
                } else {
                    $crate::macro_deps::return_user_error!(
                        "--schema is not supported for this command"
                    );
                }
                return Ok(());
            }

            let tool = <$tool as $crate::FfxTool>::from_env(env.clone(), cmd).await?;
            env.update_log_file(tool.log_basename())?;
            let res = $crate::FfxMain::main(tool, writer).await;
            // The env must not be dropped entirely until after the main function has completed, as
            // the underlying backing connection is kept inside the environment (in the case of
            // direct connections via `crate::connector::DefaultConnector`.
            env.maybe_wrap_connection_errors(res).await
        }

        pub fn ffx_plugin_is_machine_supported() -> bool {
            use $crate::macro_deps::ffx_writer::ToolIO;
            <<$tool as $crate::FfxMain>::Writer as ToolIO>::is_machine_supported()
        }

        pub fn ffx_plugin_has_schema() -> bool {
            use $crate::macro_deps::ffx_writer::ToolIO;
            <<$tool as $crate::FfxMain>::Writer as ToolIO>::has_schema()
        }
    };
}

#[cfg(test)]
mod tests {
    use crate::subtool::{FhoHandler, ToolCommand};
    use crate::testing::*;
    use argh::FromArgs;
    use ffx_command::FfxCommandLine;
    use std::sync::Arc;

    // The main testing part will happen in the `main()` function of the tool.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_run_fake_tool_with_legacy_shim() {
        let _config_env = ffx_config::test_init().await.expect("Initializing test environment");
        let injector = ToolEnv::new().take_injector();
        let ffx_cmd_line = FfxCommandLine::new(None, &["ffx", "fake", "stuff"]).unwrap();
        let tool_cmd = ToolCommand::<FakeTool>::from_args(
            &Vec::from_iter(ffx_cmd_line.cmd_iter()),
            &Vec::from_iter(ffx_cmd_line.subcmd_iter()),
        )
        .unwrap();

        embedded_plugin!(FakeTool);

        assert_eq!(
            SIMPLE_CHECK_COUNTER.with(|counter| *counter.borrow()),
            0,
            "tool pre-check should not have been called yet"
        );

        assert!(
            !ffx_plugin_is_machine_supported(),
            "Test plugin should not support machine output"
        );

        assert!(!ffx_plugin_has_schema(), "Test plugin should not support machine output schema");

        let fake_tool = match tool_cmd.subcommand {
            FhoHandler::Standalone(t) => t,
            FhoHandler::Metadata(_) => panic!("Not testing metadata generation"),
        };

        let injector: Arc<dyn ffx_core::Injector> = Arc::new(injector);
        ffx_plugin_impl(&Some(injector), fake_tool).await.expect("Plugin to run successfully");

        assert_eq!(
            SIMPLE_CHECK_COUNTER.with(|counter| *counter.borrow()),
            1,
            "tool pre-check should have been called once"
        );
    }
}
