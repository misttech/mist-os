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
            injector: &::std::sync::Arc<dyn ffx_core::Injector>,
            cmd: <$tool as $crate::FfxTool>::Command,
        ) -> ::anyhow::Result<()> {
            // TODO(120283): anyhow is used directly here to keep using the
            // global anyhow include in plugin libs. When enough plugins have
            // migrated to make it worthwhile to remove the default include, this
            // should switch back to pulling any anyhow-related types from
            // $crate::macro_deps::anyhow.
            use ::anyhow::Context;
            #[allow(unused_imports)]
            use $crate::macro_deps::{argh, global_env_context, FfxCommandLine};

            let ffx = FfxCommandLine::from_env()?;
            let context = global_env_context().context("Loading global environment context")?;
            let injector = injector.clone();

            let env = $crate::FhoEnvironment { ffx, context, injector };

            let writer = $crate::TryFromEnv::try_from_env(&env).await?;
            let tool = <$tool as $crate::FfxTool>::from_env(env, cmd).await?;
            match $crate::FfxMain::main(tool, writer).await {
                Ok(ok) => Ok(ok),
                Err($crate::Error::User(err)) => Err(err),
                Err($crate::Error::Unexpected(err)) => Err(err),
                other => other.context("Running command (unexpected error type)"),
            }
        }

        pub fn ffx_plugin_is_machine_supported() -> bool {
            use $crate::macro_deps::ffx_writer::ToolIO;
            <<$tool as $crate::FfxMain>::Writer as ToolIO>::is_machine_supported()
        }
    };
}

#[cfg(test)]
mod tests {
    use crate::{
        subtool::{FhoHandler, ToolCommand},
        testing::*,
    };
    use argh::FromArgs;
    use ffx_command::FfxCommandLine;
    use std::sync::Arc;

    // The main testing part will happen in the `main()` function of the tool.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_run_fake_tool_with_legacy_shim() {
        let _config_env = ffx_config::test_init().await.expect("Initializing test environment");
        let injector = ToolEnv::new()
            .writer_closure(|| async { Ok(ffx_writer::Writer::new(None)) })
            .take_injector();
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

        let fake_tool = match tool_cmd.subcommand {
            FhoHandler::Standalone(t) => t,
            FhoHandler::Metadata(_) => panic!("Not testing metadata generation"),
        };

        let injector: Arc<dyn ffx_core::Injector> = Arc::new(injector);
        ffx_plugin_impl(&injector, fake_tool).await.expect("Plugin to run successfully");

        assert_eq!(
            SIMPLE_CHECK_COUNTER.with(|counter| *counter.borrow()),
            1,
            "tool pre-check should have been called once"
        );
    }
}
