// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use cfg_if::cfg_if;
use errors::ffx_bail;
use ffx_core::ffx_plugin;
use ffx_emulator_commands::EngineOption;
use ffx_emulator_config::EngineState;
use ffx_emulator_list_args::ListCommand;

#[cfg(test)]
use mockall::{automock, predicate::*};

// Redeclare some methods we use from other crates so that
// we can mock them for tests.
#[cfg_attr(test, automock)]
#[allow(dead_code)]
mod modules {
    use super::*;

    pub(super) async fn get_all_instances() -> Result<Vec<String>> {
        ffx_emulator_common::instances::get_all_instances().await
    }

    pub(super) async fn get_engine_by_name(name: &mut Option<String>) -> Result<EngineOption> {
        ffx_emulator_commands::get_engine_by_name(name).await
    }
}

// if we're testing, use the mocked methods, otherwise use the
// ones from the other crates.
cfg_if! {
    if #[cfg(test)] {
        use self::mock_modules::get_all_instances;
        use self::mock_modules::get_engine_by_name;
    } else {
        use self::modules::get_all_instances;
        use self::modules::get_engine_by_name;
    }
}

// TODO(fxbug.dev/94232): Update this error message once shut down is more robust.
const BROKEN_MESSAGE: &str = r#"
One or more emulators are in a 'Broken' state. This is an uncommon state, but usually happens if
the Fuchsia source tree or SDK is updated while the emulator is still running. Communication with
a "Broken" emulator may still be possible, but errors will be encountered for any further `ffx emu`
commands. Running `ffx emu stop` will not shut down a broken emulator (this should be fixed as part
of fxbug.dev/94232), but it will clear that emulator's state from the system, so this error won't
appear anymore.
"#;

#[ffx_plugin()]
pub async fn list(cmd: ListCommand) -> Result<()> {
    exec_list_impl(&mut std::io::stdout(), &mut std::io::stderr(), cmd).await
}

/// Entry point for the list command that allows specifying the writer for the output.
pub async fn exec_list_impl<W: std::io::Write, E: std::io::Write>(
    writer: &mut W,
    error_writer: &mut E,
    cmd: ListCommand,
) -> Result<()> {
    let instance_list: Vec<Option<String>> = match get_all_instances().await {
        Ok(list) => list.into_iter().map(|v| Some(v)).collect(),
        Err(e) => ffx_bail!("Error encountered looking up emulator instances: {:?}", e),
    };
    let mut broken = false;
    for mut some_name in instance_list {
        match get_engine_by_name(&mut some_name).await {
            Ok(EngineOption::DoesExist(engine)) => {
                let name = some_name.unwrap();
                let state = format!("[{}]", engine.engine_state());
                if engine.engine_state() != EngineState::Running && cmd.only_running {
                    continue;
                } else {
                    writeln!(writer, "{:16}{}", state, name)?;
                }
            }
            Ok(EngineOption::DoesNotExist(_)) | Err(_) => {
                writeln!(
                    writer,
                    "[Broken]        {}",
                    some_name.unwrap_or("<unspecified>".to_string())
                )?;
                broken = true;
                continue;
            }
        };
    }
    if broken {
        writeln!(error_writer, "{}", BROKEN_MESSAGE)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use async_trait::async_trait;
    use ffx_emulator_config::{
        EmulatorConfiguration, EmulatorEngine, EngineConsoleType, EngineType, ShowDetail,
    };
    use fidl_fuchsia_developer_ffx as ffx;
    use lazy_static::lazy_static;
    use std::{
        process::Command,
        str,
        sync::{Mutex, MutexGuard},
    };

    // Since we are mocking global methods, we need to synchronize
    // the setting of the expectations on the mock. This is done using a Mutex.
    lazy_static! {
        static ref MTX: Mutex<()> = Mutex::new(());
    }

    // When a test panics, it will poison the Mutex. Since we don't actually
    // care about the state of the data we ignore that it is poisoned and grab
    // the lock regardless.  If you just do `let _m = &MTX.lock().unwrap()`, one
    // test panicking will cause all other tests that try and acquire a lock on
    // that Mutex to also panic.
    fn get_lock(m: &'static Mutex<()>) -> MutexGuard<'static, ()> {
        match m.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// TestEngine is a test struct for implementing the EmulatorEngine trait
    /// Currently this one only exposes the running flag which is returned from
    /// EmulatorEngine::is_running().
    pub struct TestEngine {
        pub running_flag: bool,
        pub engine_state: EngineState,
    }

    #[async_trait]
    impl EmulatorEngine for TestEngine {
        async fn start(&mut self, _: Command, _: &ffx::TargetCollectionProxy) -> Result<i32> {
            todo!()
        }
        async fn stop(&mut self, _: &ffx::TargetCollectionProxy) -> Result<()> {
            todo!()
        }
        fn show(&self, _: Vec<ShowDetail>) {
            todo!()
        }
        async fn stage(&mut self) -> Result<()> {
            todo!()
        }
        fn configure(&mut self) -> Result<()> {
            todo!()
        }
        fn engine_state(&self) -> EngineState {
            self.engine_state
        }
        fn engine_type(&self) -> EngineType {
            EngineType::default()
        }
        fn is_running(&mut self) -> bool {
            self.running_flag
        }
        fn build_emulator_cmd(&self) -> Command {
            todo!()
        }
        async fn load_emulator_binary(&mut self) -> Result<()> {
            todo!()
        }
        fn emu_config(&self) -> &EmulatorConfiguration {
            todo!()
        }
        fn emu_config_mut(&mut self) -> &mut EmulatorConfiguration {
            todo!()
        }
        fn attach(&self, _console: EngineConsoleType) -> Result<()> {
            todo!()
        }
        fn save_to_disk(&self) -> Result<()> {
            todo!()
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_empty_list() -> Result<()> {
        // get the lock for the mock, it is released when
        // the test exits.
        let _m = get_lock(&MTX);

        // no existing instances
        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| Ok(vec![]));
        let cmd = ListCommand { only_running: false };

        let engine_ctx = mock_modules::get_engine_by_name_context();
        engine_ctx.expect().times(0);

        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];
        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        assert!(stdout.is_empty());
        assert!(stderr.is_empty());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_new_list() -> Result<()> {
        // get the lock for the mock, it is released when
        // the test exits.
        let _m = get_lock(&MTX);

        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| Ok(vec!["notrunning_emu".to_string()]));
        let cmd = ListCommand { only_running: false };

        let engine_ctx = mock_modules::get_engine_by_name_context();
        engine_ctx.expect().returning(|_| {
            Ok(EngineOption::DoesExist(Box::new(TestEngine {
                running_flag: false,
                engine_state: EngineState::New,
            })))
        });

        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];
        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        let stdout_expected = "[new]           notrunning_emu\n";
        assert_eq!(str::from_utf8(&stdout)?, stdout_expected);
        assert!(stderr.is_empty());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_configured_list() -> Result<()> {
        // get the lock for the mock, it is released when
        // the test exits.
        let _m = get_lock(&MTX);

        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| Ok(vec!["notrunning_emu".to_string()]));
        let cmd = ListCommand { only_running: false };

        let engine_ctx = mock_modules::get_engine_by_name_context();
        engine_ctx.expect().returning(|_| {
            Ok(EngineOption::DoesExist(Box::new(TestEngine {
                running_flag: false,
                engine_state: EngineState::Configured,
            })))
        });

        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];
        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        let stdout_expected = "[configured]    notrunning_emu\n";
        assert_eq!(str::from_utf8(&stdout)?, stdout_expected);
        assert!(stderr.is_empty());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_staged_list() -> Result<()> {
        // get the lock for the mock, it is released when
        // the test exits.
        let _m = get_lock(&MTX);

        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| Ok(vec!["notrunning_emu".to_string()]));
        let cmd = ListCommand { only_running: false };

        let engine_ctx = mock_modules::get_engine_by_name_context();
        engine_ctx.expect().returning(|_| {
            Ok(EngineOption::DoesExist(Box::new(TestEngine {
                running_flag: false,
                engine_state: EngineState::Staged,
            })))
        });

        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];
        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        let stdout_expected = "[staged]        notrunning_emu\n";
        assert_eq!(str::from_utf8(&stdout)?, stdout_expected);
        assert!(stderr.is_empty());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_running_list() -> Result<()> {
        // get the lock for the mock, it is released when
        // the test exits.
        let _m = get_lock(&MTX);

        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| Ok(vec!["running_emu".to_string()]));
        let cmd = ListCommand { only_running: false };

        let engine_ctx = mock_modules::get_engine_by_name_context();
        engine_ctx.expect().returning(|_| {
            Ok(EngineOption::DoesExist(Box::new(TestEngine {
                running_flag: true,
                engine_state: EngineState::Running,
            })))
        });

        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];

        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        let stdout_expected = "[running]       running_emu\n";
        assert_eq!(str::from_utf8(&stdout)?, stdout_expected);
        assert!(stderr.is_empty());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_running_flag() -> Result<()> {
        // get the lock for the mock, it is released when
        // the test exits.
        let _m = get_lock(&MTX);

        let ctx = mock_modules::get_all_instances_context();
        ctx.expect()
            .returning(|| {
                Ok(vec![
                    "new_emu".to_string(),
                    "config_emu".to_string(),
                    "staged_emu".to_string(),
                    "running_emu".to_string(),
                    "error_emu".to_string(),
                ])
            })
            .times(2);

        // First make sure it filters the non-running instances when the flag is true.
        let engine_ctx = mock_modules::get_engine_by_name_context();
        engine_ctx
            .expect()
            .returning(|_| {
                Ok(EngineOption::DoesExist(Box::new(TestEngine {
                    running_flag: true,
                    engine_state: EngineState::New,
                })))
            })
            .times(1);
        engine_ctx
            .expect()
            .returning(|_| {
                Ok(EngineOption::DoesExist(Box::new(TestEngine {
                    running_flag: true,
                    engine_state: EngineState::Configured,
                })))
            })
            .times(1);
        engine_ctx
            .expect()
            .returning(|_| {
                Ok(EngineOption::DoesExist(Box::new(TestEngine {
                    running_flag: true,
                    engine_state: EngineState::Staged,
                })))
            })
            .times(1);
        engine_ctx
            .expect()
            .returning(|_| {
                Ok(EngineOption::DoesExist(Box::new(TestEngine {
                    running_flag: true,
                    engine_state: EngineState::Running,
                })))
            })
            .times(1);
        engine_ctx
            .expect()
            .returning(|_| {
                Ok(EngineOption::DoesExist(Box::new(TestEngine {
                    running_flag: true,
                    engine_state: EngineState::Error,
                })))
            })
            .times(1);

        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];

        let cmd = ListCommand { only_running: true };
        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        let stdout_expected = "[running]       running_emu\n";
        assert_eq!(str::from_utf8(&stdout)?, stdout_expected);
        assert!(stderr.is_empty());

        // Then make sure it leaves them in when the flag is false.
        engine_ctx
            .expect()
            .returning(|_| {
                Ok(EngineOption::DoesExist(Box::new(TestEngine {
                    running_flag: true,
                    engine_state: EngineState::New,
                })))
            })
            .times(1);
        engine_ctx
            .expect()
            .returning(|_| {
                Ok(EngineOption::DoesExist(Box::new(TestEngine {
                    running_flag: true,
                    engine_state: EngineState::Configured,
                })))
            })
            .times(1);
        engine_ctx
            .expect()
            .returning(|_| {
                Ok(EngineOption::DoesExist(Box::new(TestEngine {
                    running_flag: true,
                    engine_state: EngineState::Staged,
                })))
            })
            .times(1);
        engine_ctx
            .expect()
            .returning(|_| {
                Ok(EngineOption::DoesExist(Box::new(TestEngine {
                    running_flag: true,
                    engine_state: EngineState::Running,
                })))
            })
            .times(1);
        engine_ctx
            .expect()
            .returning(|_| {
                Ok(EngineOption::DoesExist(Box::new(TestEngine {
                    running_flag: true,
                    engine_state: EngineState::Error,
                })))
            })
            .times(1);

        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];

        let cmd = ListCommand { only_running: false };
        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        assert!(str::from_utf8(&stdout)?.contains("new_emu"));
        assert!(str::from_utf8(&stdout)?.contains("config_emu"));
        assert!(str::from_utf8(&stdout)?.contains("staged_emu"));
        assert!(str::from_utf8(&stdout)?.contains("running_emu"));
        assert!(str::from_utf8(&stdout)?.contains("error_emu"));
        assert!(stderr.is_empty());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_error_list() -> Result<()> {
        // get the lock for the mock, it is released when
        // the test exits.
        let _m = get_lock(&MTX);

        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| Ok(vec!["running_emu".to_string()]));
        let cmd = ListCommand { only_running: false };

        let engine_ctx = mock_modules::get_engine_by_name_context();
        engine_ctx.expect().returning(|_| {
            Ok(EngineOption::DoesExist(Box::new(TestEngine {
                running_flag: true,
                engine_state: EngineState::Error,
            })))
        });

        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];

        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        let stdout_expected = "[error]         running_emu\n";
        assert_eq!(str::from_utf8(&stdout)?, stdout_expected);
        assert!(stderr.is_empty());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_broken_list() -> Result<()> {
        // get the lock for the mock, it is released when
        // the test exits.
        let _m = get_lock(&MTX);

        let ctx = mock_modules::get_all_instances_context();
        ctx.expect().returning(|| Ok(vec!["error_emu".to_string()]));
        let cmd = ListCommand { only_running: false };

        let engine_ctx = mock_modules::get_engine_by_name_context();
        engine_ctx.expect().returning(|_| Err(anyhow!("This instance cannot be parsed")));

        let mut stdout: Vec<u8> = vec![];
        let mut stderr: Vec<u8> = vec![];

        exec_list_impl(&mut stdout, &mut stderr, cmd).await?;

        let stdout_expected = "[Broken]        error_emu\n";
        assert_eq!(str::from_utf8(&stdout)?, stdout_expected);
        assert_eq!(str::from_utf8(&stderr)?, format!("{}\n", BROKEN_MESSAGE));
        Ok(())
    }
}
