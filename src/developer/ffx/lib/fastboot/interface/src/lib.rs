// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod fastboot_interface;
pub mod fastboot_proxy;
pub mod interface_factory;

pub mod test {
    use super::fastboot_interface::{
        Fastboot, FastbootError, FastbootInterface, RebootEvent, UploadProgress, Variable,
    };
    use anyhow::{anyhow, Result};
    use async_trait::async_trait;
    use chrono::Duration;
    use std::collections::HashMap;
    use std::default::Default;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc::Sender;

    #[derive(Default, Debug)]
    pub struct FakeServiceCommands {
        pub staged_files: Vec<String>,
        pub oem_commands: Vec<String>,
        pub bootloader_reboots: usize,
        pub boots: usize,
        /// Variable => (Value, Call Count)
        variables: HashMap<String, (String, u32)>,
    }

    impl FakeServiceCommands {
        /// Sets the provided variable to the given value preserving the past
        /// call count.
        pub fn set_var(&mut self, var: String, value: String) {
            match self.variables.get_mut(&var) {
                Some(v) => {
                    let last_call_count = v.1;
                    self.variables.insert(var, (value, last_call_count));
                }
                None => {
                    self.variables.insert(var, (value, 0));
                }
            }
        }

        /// Returns the number of times a variable was retrieved from the
        /// fake if the variable has been set, panics otherwise.
        pub fn get_var_call_count(&self, var: String) -> u32 {
            match self.variables.get(&var) {
                Some(v) => v.1,
                None => panic!("Requested variable: {} was not set", var),
            }
        }
    }

    #[derive(Debug)]
    pub struct TestFastbootInterface {
        state: Arc<Mutex<FakeServiceCommands>>,
    }

    impl TestFastbootInterface {
        pub fn new(state: Arc<Mutex<FakeServiceCommands>>) -> Self {
            Self { state }
        }
    }

    impl FastbootInterface for TestFastbootInterface {}

    #[async_trait(?Send)]
    impl Fastboot for TestFastbootInterface {
        async fn get_var(&mut self, name: &str) -> Result<String, FastbootError> {
            let mut state = self.state.lock().unwrap();
            match state.variables.get_mut(name) {
                Some(var) => {
                    var.1 += 1;
                    Ok(var.0.clone())
                }
                None => {
                    panic!("Warning: requested variable: {}, which was not set", name)
                }
            }
        }

        async fn get_all_vars(&mut self, listener: Sender<Variable>) -> Result<(), FastbootError> {
            listener
                .send(Variable { name: "test".to_string(), value: "test".to_string() })
                .await
                .unwrap();
            Ok(())
        }

        async fn flash(
            &mut self,
            _partition_name: &str,
            _path: &str,
            listener: Sender<UploadProgress>,
            _timeout: Duration,
        ) -> Result<(), FastbootError> {
            listener.send(UploadProgress::OnStarted { size: 1 }).await.map_err(|e| anyhow!(e))?;
            listener
                .send(UploadProgress::OnProgress { bytes_written: 1 })
                .await
                .map_err(|e| anyhow!(e))?;
            listener.send(UploadProgress::OnFinished).await.map_err(|e| anyhow!(e))?;
            Ok(())
        }

        async fn erase(&mut self, _partition_name: &str) -> Result<(), FastbootError> {
            Ok(())
        }

        async fn boot(&mut self) -> Result<(), FastbootError> {
            let mut state = self.state.lock().unwrap();
            state.boots += 1;
            Ok(())
        }

        async fn reboot(&mut self) -> Result<(), FastbootError> {
            Ok(())
        }

        async fn reboot_bootloader(
            &mut self,
            listener: Sender<RebootEvent>,
        ) -> Result<(), FastbootError> {
            listener.send(RebootEvent::OnReboot).await.unwrap();
            let mut state = self.state.lock().unwrap();
            state.bootloader_reboots += 1;
            Ok(())
        }

        async fn continue_boot(&mut self) -> Result<(), FastbootError> {
            Ok(())
        }

        async fn get_staged(&mut self, _path: &str) -> Result<(), FastbootError> {
            Ok(())
        }

        async fn stage(
            &mut self,
            path: &str,
            _listener: Sender<UploadProgress>,
        ) -> Result<(), FastbootError> {
            let mut state = self.state.lock().unwrap();
            state.staged_files.push(path.to_string());
            Ok(())
        }

        async fn set_active(&mut self, _slot: &str) -> Result<(), FastbootError> {
            Ok(())
        }

        async fn oem(&mut self, command: &str) -> Result<(), FastbootError> {
            let mut state = self.state.lock().unwrap();
            state.oem_commands.push(command.to_string());
            Ok(())
        }
    }

    pub fn setup() -> (Arc<Mutex<FakeServiceCommands>>, TestFastbootInterface) {
        let state = Arc::new(Mutex::new(FakeServiceCommands::default()));
        let interface = TestFastbootInterface { state: state.clone() };
        (state, interface)
    }
}
