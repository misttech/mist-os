// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The qemu_base module encapsulates traits and functions specific
//! for engines using QEMU as the emulator platform.

use crate::{
    arg_templates::process_flag_template,
    qemu_based::comms::{spawn_pipe_thread, QemuSocket},
    show_output,
};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use cfg_if::cfg_if;
use emulator_instance::{
    AccelerationMode, ConsoleType, DiskImage, EmulatorConfiguration, EngineState, GuestConfig,
    NetworkingMode,
};
use errors::ffx_bail;
use ffx_emulator_common::{
    config,
    config::EMU_START_TIMEOUT,
    dump_log_to_out, host_is_mac, process,
    target::is_active,
    tuntap::{tap_ready, TAP_INTERFACE_NAME},
};
use ffx_emulator_config::{EmulatorEngine, EngineConsoleType, ShowDetail};
use ffx_ssh::SshKeyFiles;
use fidl_fuchsia_developer_ffx as ffx;
use fuchsia_async::Timer;
use serde_json::{json, Deserializer, Value};
use shared_child::SharedChild;
use std::{
    env,
    fs::{self, File},
    io::{stderr, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::Command,
    str,
    sync::{mpsc::channel, Arc},
    time::{Duration, Instant},
};
use tempfile::NamedTempFile;

#[cfg(test)]
use mockall::automock;

#[cfg_attr(test, automock)]
#[allow(dead_code)]
mod modules {
    use super::*;

    pub(crate) async fn get_host_tool(name: &str) -> Result<PathBuf> {
        let sdk = ffx_config::global_env_context()
            .context("loading global environment context")?
            .get_sdk()
            .await?;

        // Attempts to get a host tool from the SDK manifest. If it fails, falls
        // back to attempting to derive the path to the host tool binary by simply checking
        // for its existence in `ffx`'s directory.
        // TODO(fxb/99321): When issues around including aemu in the sdk are resolved, this
        // hack can be removed.
        match sdk.get_host_tool(name) {
            Ok(path) => Ok(path),
            Err(error) => {
                tracing::warn!(
                    "failed to get host tool {} from manifest. Trying local SDK dir: {}",
                    name,
                    error
                );
                let mut ffx_path = std::env::current_exe()
                    .context(format!("getting current ffx exe path for host tool {}", name))?;
                ffx_path = std::fs::canonicalize(ffx_path.clone())
                    .context(format!("canonicalizing ffx path {:?}", ffx_path))?;

                let tool_path = ffx_path
                    .parent()
                    .context(format!("ffx path missing parent {:?}", ffx_path))?
                    .join(name);

                if tool_path.exists() {
                    Ok(tool_path)
                } else {
                    bail!("Host tool '{}' not found after checking in `ffx` directory.", name);
                }
            }
        }
    }
}

cfg_if! {
    if #[cfg(test)] {
        pub(crate) use self::mock_modules::get_host_tool;
    } else {
pub(crate) use self::modules::get_host_tool;
    }
}

pub(crate) mod comms;
pub(crate) mod femu;
pub(crate) mod qemu;

const COMMAND_CONSOLE: &str = "./monitor";
const MACHINE_CONSOLE: &str = "./qmp";
const SERIAL_CONSOLE: &str = "./serial";

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct PortPair {
    pub guest: u16,
    pub host: u16,
}
/// QemuBasedEngine collects the interface for
/// emulator engine implementations that use
/// QEMU as the emulator.
/// This allows the implementation to be shared
/// across multiple engine types.
#[async_trait]
pub(crate) trait QemuBasedEngine: EmulatorEngine {
    /// Checks that the required files are present
    fn check_required_files(&self, guest: &GuestConfig) -> Result<()> {
        let kernel_path = &guest.kernel_image;
        let zbi_path = &guest.zbi_image;
        let disk_image_path = &guest.disk_image;

        if !kernel_path.exists() {
            bail!("kernel file {:?} does not exist.", kernel_path);
        }
        if !zbi_path.exists() {
            bail!("zbi file {:?} does not exist.", zbi_path);
        }
        if let Some(file_path) = disk_image_path.as_ref() {
            if !file_path.exists() {
                bail!("disk image file {:?} does not exist.", file_path);
            }
        }
        Ok(())
    }

    /// Stages the source image files in an instance specific directory.
    /// Also resizes the fvms to the desired size and adds the authorized
    /// keys to the zbi.
    /// Returns an updated GuestConfig instance with the file paths set to
    /// the instance paths.
    async fn stage_image_files(
        instance_name: &str,
        emu_config: &EmulatorConfiguration,
        reuse: bool,
    ) -> Result<GuestConfig> {
        let mut updated_guest = emu_config.guest.clone();

        // Create the data directory if needed.
        let mut instance_root: PathBuf =
            ffx_config::query(config::EMU_INSTANCE_ROOT_DIR).get_file().await?;
        instance_root.push(instance_name);
        fs::create_dir_all(&instance_root)?;

        let kernel_name = emu_config.guest.kernel_image.file_name().ok_or_else(|| {
            anyhow!("cannot read kernel file name '{:?}'", emu_config.guest.kernel_image)
        });
        let kernel_path = instance_root.join(kernel_name?);
        if kernel_path.exists() && reuse {
            tracing::debug!("Using existing file for {:?}", kernel_path.file_name().unwrap());
        } else {
            fs::copy(&emu_config.guest.kernel_image, &kernel_path)
                .context("cannot stage kernel file")?;
        }

        let zbi_path = instance_root.join(
            emu_config
                .guest
                .zbi_image
                .file_name()
                .ok_or_else(|| anyhow!("cannot read zbi file name"))?,
        );

        if zbi_path.exists() && reuse {
            tracing::debug!("Using existing file for {:?}", zbi_path.file_name().unwrap());
            // TODO(fxbug.dev/112577): Make a decision to reuse zbi with no modifications or not.
            // There is the potential that the ssh keys have changed, or the ip address
            // of the host interface has changed, which will cause the connection
            // to the emulator instance to fail.
        } else {
            // Add the authorized public keys to the zbi image to enable SSH access to
            // the guest.
            Self::embed_boot_data(&emu_config.guest.zbi_image, &zbi_path)
                .await
                .context("cannot embed boot data")?;
        }

        if let Some(disk_image) = &emu_config.guest.disk_image {
            let src_path = disk_image.as_ref();
            let dest_path = instance_root.join(
                src_path.file_name().ok_or_else(|| anyhow!("cannot read disk image file name"))?,
            );

            if dest_path.exists() && reuse {
                tracing::debug!("Using existing file for {:?}", dest_path.file_name().unwrap());
            } else {
                let original_size: u64 = src_path.metadata()?.len();

                tracing::debug!("Disk image original size: {}", original_size);
                tracing::debug!(
                    "Disk image target size from product bundle {:?}",
                    emu_config.device.storage
                );

                let mut target_size =
                    emu_config.device.storage.as_bytes().expect("get device storage size");

                // The disk image needs to be expanded in size in order to make room
                // for the creation of the data volume. If the original
                // size is larger than the target size, update the target size
                // to 1.1 times the size of the original file.
                if target_size < original_size {
                    let new_target_size: u64 = original_size + (original_size / 10);
                    tracing::warn!("Disk image original size is larger than target size.");
                    tracing::warn!("Forcing target size to {new_target_size}");
                    target_size = new_target_size;
                }

                // The method of resizing is different, depending on the type of the disk image.
                match disk_image {
                    DiskImage::Fvm(_) => {
                        fs::copy(src_path, &dest_path).context("cannot stage disk image file")?;
                        Self::fvm_extend(&dest_path, target_size).await?;
                    }
                    DiskImage::Fxfs(_) => {
                        let tmp = NamedTempFile::new_in(&instance_root)?;
                        fs::copy(src_path, tmp.path()).context("cannot stage Fxfs image")?;
                        if original_size < target_size {
                            // Resize the image if needed.
                            tmp.as_file()
                                .set_len(target_size)
                                .context(format!("Failed to temp file to {} bytes", target_size))?;
                        }
                        tmp.persist(&dest_path).context(format!(
                            "Failed to persist temp Fxfs image to {:?}",
                            dest_path
                        ))?;
                    }
                };
            }

            // Update the guest config to reference the staged disk image.
            updated_guest.disk_image = match disk_image {
                DiskImage::Fvm(_) => Some(DiskImage::Fvm(dest_path)),
                DiskImage::Fxfs(_) => Some(DiskImage::Fxfs(dest_path)),
            };
        } else {
            updated_guest.disk_image = None;
        }

        updated_guest.kernel_image = kernel_path;
        updated_guest.zbi_image = zbi_path;
        Ok(updated_guest)
    }

    async fn fvm_extend(dest_path: &Path, target_size: u64) -> Result<()> {
        let fvm_tool =
            get_host_tool(config::FVM_HOST_TOOL).await.context("cannot locate fvm tool")?;
        let mut resize_command = Command::new(fvm_tool);

        resize_command.arg(&dest_path).arg("extend").arg("--length").arg(target_size.to_string());
        tracing::debug!("FVM Running command to resize: {:?}", &resize_command);

        let resize_result = resize_command.output()?;

        tracing::debug!("FVM command result: {resize_result:?}");

        if !resize_result.status.success() {
            bail!("Error resizing fvm: {}", str::from_utf8(&resize_result.stderr)?);
        }
        Ok(())
    }

    /// embed_boot_data adds authorized_keys for ssh access to the zbi boot image file.
    /// If mdns_info is Some(), it is also added. This mdns configuration is
    /// read by Fuchsia mdns service and used instead of the default configuration.
    async fn embed_boot_data(src: &PathBuf, dest: &PathBuf) -> Result<()> {
        let zbi_tool = get_host_tool(config::ZBI_HOST_TOOL).await.context("ZBI tool is missing")?;
        let ssh_keys = SshKeyFiles::load().await.context("finding ssh authorized_keys file.")?;
        ssh_keys.create_keys_if_needed().context("create ssh keys if needed")?;
        let auth_keys = ssh_keys.authorized_keys.display().to_string();
        if !ssh_keys.authorized_keys.exists() {
            bail!("No authorized_keys found to configure emulator. {} does not exist.", auth_keys);
        }
        if src == dest {
            return Err(anyhow!("source and dest zbi paths cannot be the same."));
        }

        let replace_str = format!("data/ssh/authorized_keys={}", auth_keys);

        let mut zbi_command = Command::new(zbi_tool);
        zbi_command.arg("-o").arg(dest).arg("--replace").arg(src).arg("-e").arg(replace_str);

        // added last.
        zbi_command.arg("--type=entropy:64").arg("/dev/urandom");

        let zbi_command_output = zbi_command.output()?;

        if !zbi_command_output.status.success() {
            bail!("Error embedding boot data: {}", str::from_utf8(&zbi_command_output.stderr)?);
        }
        Ok(())
    }

    fn validate_network_flags(&self, emu_config: &EmulatorConfiguration) -> Result<()> {
        match emu_config.host.networking {
            NetworkingMode::None => {
                // Check for console/monitor.
                if emu_config.runtime.console == ConsoleType::None {
                    bail!(
                        "Running without networking enabled and no interactive console;\n\
                        there will be no way to communicate with this emulator.\n\
                        Restart with --console/--monitor or with networking enabled to proceed."
                    );
                }
            }
            NetworkingMode::Auto => {
                // Shouldn't be possible to land here.
                bail!("Networking mode is unresolved after configuration.");
            }
            NetworkingMode::Tap => {
                // Officially, MacOS tun/tap is unsupported. tap_ready() uses the "ip" command to
                // retrieve details about the target interface, but "ip" is not installed on macs
                // by default. That means, if tap_ready() is called on a MacOS host, it returns a
                // Result::Error, which would cancel emulation. However, if an end-user sets up
                // tun/tap on a MacOS host we don't want to block that, so we check the OS here
                // and make it a warning to run on MacOS instead.
                if host_is_mac() {
                    eprintln!(
                        "Tun/Tap networking mode is not currently supported on MacOS. \
                        You may experience errors with your current configuration."
                    );
                } else {
                    // tap_ready() has some good error reporting, so just return the Result.
                    return tap_ready();
                }
            }
            NetworkingMode::User => (),
        }
        Ok(())
    }

    async fn stage(&mut self) -> Result<()> {
        let emu_config = self.emu_config_mut();
        let name = emu_config.runtime.name.clone();
        let reuse = emu_config.runtime.reuse;

        emu_config.guest = Self::stage_image_files(&name, emu_config, reuse)
            .await
            .context("could not stage image files")?;

        // This is done to avoid running emu in the same directory as the kernel or other files
        // that are used by qemu. If the multiboot.bin file is in the current directory, it does
        // not start correctly. This probably could be temporary until we know the images loaded
        // do not have files directly in $sdk_root.
        env::set_current_dir(emu_config.runtime.instance_directory.parent().unwrap())
            .context("problem changing directory to instance dir")?;

        emu_config.flags = process_flag_template(emu_config)
            .context("Failed to process the flags template file.")?;

        Ok(())
    }

    async fn run(
        &mut self,
        mut emulator_cmd: Command,
        proxy: &ffx::TargetCollectionProxy,
    ) -> Result<i32> {
        if self.emu_config().runtime.console == ConsoleType::None {
            let stdout = File::create(&self.emu_config().host.log)
                .context(format!("Couldn't open log file {:?}", &self.emu_config().host.log))?;
            let stderr = stdout
                .try_clone()
                .context("Failed trying to clone stdout for the emulator process.")?;
            emulator_cmd.stdout(stdout).stderr(stderr);
            println!("Logging to {:?}", &self.emu_config().host.log);
        }

        // If using TAP, check for an upscript to run.
        if let Some(script) = match &self.emu_config().host.networking {
            NetworkingMode::Tap => &self.emu_config().runtime.upscript,
            _ => &None,
        } {
            let status = Command::new(script)
                .arg(TAP_INTERFACE_NAME)
                .status()
                .context(format!("Problem running upscript '{}'", &script.display()))?;
            if !status.success() {
                return Err(anyhow!(
                    "Upscript {} returned non-zero exit code {}",
                    script.display(),
                    status.code().map_or("None".to_string(), |v| format!("{}", v))
                ));
            }
        }

        let shared_process = SharedChild::spawn(&mut emulator_cmd)?;
        let child_arc = Arc::new(shared_process);

        self.set_pid(child_arc.id());
        self.set_engine_state(EngineState::Running);

        if self.emu_config().host.networking == NetworkingMode::User {
            // Capture the port mappings for user mode networking.
            let now = fuchsia_async::Time::now();
            self.read_port_mappings().await?;
            let elapsed_ms = now.elapsed().as_millis();
            tracing::debug!("reading port mappings took {elapsed_ms}ms");
        } else {
            self.save_to_disk()
                .await
                .context("Failed to write the emulation configuration file to disk.")?;
        }

        if self.emu_config().runtime.debugger {
            println!("The emulator will wait for a debugger to attach before starting up.");
            println!("Attach to process {} to continue launching the emulator.", self.get_pid());
        }

        if self.emu_config().runtime.console == ConsoleType::Monitor
            || self.emu_config().runtime.console == ConsoleType::Console
        {
            // When running with '--monitor' or '--console' mode, the user is directly interacting
            // with the emulator console, or the guest console. Therefore wait until the
            // execution of QEMU or AEMU terminates.
            match fuchsia_async::unblock(move || process::monitored_child_process(&child_arc)).await
            {
                Ok(_) => {
                    return Ok(0);
                }
                Err(e) => {
                    if let Some(stop_error) = self.stop_emulator().await.err() {
                        tracing::debug!(
                            "Error encountered in stop when handling failed launch: {:?}",
                            stop_error
                        );
                    }
                    ffx_bail!("Emulator launcher did not terminate properly, error: {}", e)
                }
            }
        } else if !self.emu_config().runtime.startup_timeout.is_zero() {
            // Wait until the emulator is considered "active" before returning to the user.
            let startup_timeout = self.emu_config().runtime.startup_timeout.as_secs();
            print!("Waiting for Fuchsia to start (up to {} seconds).", startup_timeout);
            tracing::debug!("Waiting for Fuchsia to start (up to {} seconds)...", startup_timeout);
            let name = self.emu_config().runtime.name.clone();
            let start = Instant::now();

            while start.elapsed().as_secs() <= startup_timeout {
                if is_active(proxy, &name).await {
                    println!("\nEmulator is ready.");
                    tracing::debug!(
                        "Emulator is ready after {} seconds.",
                        start.elapsed().as_secs()
                    );
                    return Ok(0);
                }

                // Perform a check to make sure the process is still alive, otherwise report
                // failure to launch.
                if !self.is_running().await {
                    tracing::error!(
                        "Emulator process failed to launch, but we don't know the cause. \
                        Check the emulator log, or look for a crash log."
                    );
                    eprintln!(
                        "\nEmulator process failed to launch, but we don't know the cause. \
                        Printing the contents of the emulator log...\n"
                    );
                    match dump_log_to_out(&self.emu_config().host.log, &mut stderr()) {
                        Ok(_) => (),
                        Err(e) => eprintln!("Couldn't print the log: {:?}", e),
                    };
                    self.set_engine_state(EngineState::Staged);
                    self.save_to_disk()
                        .await
                        .context("Failed to write the emulation configuration file to disk.")?;

                    return Ok(1);
                }

                // Output a little status indicator to show we haven't gotten stuck.
                // Note that we discard the result on the flush call; it's not important enough
                // that we flushed the output stream to derail the launch.
                print!(".");
                std::io::stdout().flush().ok();

                // Sleep for a bit to allow the instance to make progress
                Timer::new(Duration::from_secs(1)).await;
            }

            // If we're here, it means the emulator did not start within the timeout.

            eprintln!();
            eprintln!(
                "After {} seconds, the emulator has not responded to network queries.",
                self.emu_config().runtime.startup_timeout.as_secs()
            );
            if self.is_running().await {
                eprintln!("The emulator process is still running (pid {}).", self.get_pid());
                eprintln!(
                    "The emulator is configured to {} network access.",
                    match self.emu_config().host.networking {
                        NetworkingMode::Tap => "use tun/tap-based",
                        NetworkingMode::User => "use user-mode/port-mapped",
                        NetworkingMode::None => "disable all",
                        NetworkingMode::Auto => bail!(
                            "Auto Networking mode should not be possible after staging \
                            is complete. Configuration is corrupt; bailing out."
                        ),
                    }
                );
                eprintln!(
                    "Hardware acceleration is {}.",
                    if self.emu_config().host.acceleration == AccelerationMode::Hyper {
                        "enabled"
                    } else {
                        "disabled, which significantly slows down the emulator"
                    }
                );
                eprintln!(
                    "You can execute `ffx target list` to keep monitoring the device, \
                    or `ffx emu stop` to terminate it."
                );
                eprintln!(
                    "You can also change the timeout if you keep encountering this \
                    message by executing `ffx config set {} <seconds>`.",
                    EMU_START_TIMEOUT
                );
            } else {
                eprintln!();
                eprintln!(
                    "Emulator process failed to launch, but we don't know the cause. \
                    Printing the contents of the emulator log...\n"
                );
                match dump_log_to_out(&self.emu_config().host.log, &mut std::io::stderr()) {
                    Ok(_) => (),
                    Err(e) => eprintln!("Couldn't print the log: {:?}", e),
                };
            }

            tracing::warn!("Emulator did not respond to a health check before timing out.");
            return Ok(1);
        }
        Ok(0)
    }

    fn show(&self, details: Vec<ShowDetail>) -> Vec<ShowDetail> {
        let mut results: Vec<ShowDetail> = vec![];
        for segment in details {
            match segment {
                ShowDetail::Raw { .. } => {
                    results.push(ShowDetail::Raw { config: Some(self.emu_config().clone()) })
                }
                ShowDetail::Cmd { .. } => {
                    results.push(show_output::command(&self.build_emulator_cmd()))
                }
                ShowDetail::Config { .. } => results.push(show_output::config(self.emu_config())),
                ShowDetail::Device { .. } => results.push(show_output::device(self.emu_config())),
                ShowDetail::Net { .. } => results.push(show_output::net(self.emu_config())),
                _ => {}
            };
        }
        results
    }

    async fn stop_emulator(&mut self) -> Result<()> {
        if self.is_running().await {
            println!("Terminating running instance {:?}", self.get_pid());
            if let Some(terminate_error) = process::terminate(self.get_pid()).err() {
                tracing::warn!("Error encountered terminating process: {:?}", terminate_error);
            }
        }
        self.set_engine_state(EngineState::Staged);
        self.save_to_disk().await
    }

    /// Access to the engine's pid field.
    fn set_pid(&mut self, pid: u32);
    fn get_pid(&self) -> u32;

    /// Access to the engine's engine_state field.
    fn set_engine_state(&mut self, state: EngineState);
    fn get_engine_state(&self) -> EngineState;

    /// Attach to emulator's console socket.
    fn attach_to(&self, path: &Path, console: EngineConsoleType) -> Result<()> {
        let console_path = self.get_path_for_console_type(path, console);
        let mut socket = QemuSocket::new(&console_path);
        socket.connect().context("Connecting to console.")?;
        let stream = socket.stream().ok_or_else(|| anyhow!("No socket connected."))?;
        let (tx, rx) = channel();

        let _t1 = spawn_pipe_thread(std::io::stdin(), stream.try_clone()?, tx.clone());
        let _t2 = spawn_pipe_thread(stream.try_clone()?, std::io::stdout(), tx);

        // Now that the threads are reading and writing, we wait for one to send back an error.
        let error = rx.recv()?;
        eprintln!("{:?}", error);
        stream.shutdown(Shutdown::Both).context("Shutting down stream.")?;
        Ok(())
    }

    fn get_path_for_console_type(&self, path: &Path, console: EngineConsoleType) -> PathBuf {
        path.join(match console {
            EngineConsoleType::Command => COMMAND_CONSOLE,
            EngineConsoleType::Machine => MACHINE_CONSOLE,
            EngineConsoleType::Serial => SERIAL_CONSOLE,
            EngineConsoleType::None => panic!("No path exists for EngineConsoleType::None"),
        })
    }

    /// Connect to the qmp socket for the emulator instance and read the port mappings.
    /// User mode networking needs to map guest TCP ports to host ports. This can be done by
    /// specifying the guest port and either a preassigned port from the command line, or
    /// leaving the host port unassigned, and a port is assigned by the emulator at startup.
    ///
    /// This method waits for the QMP socket to be open, then reads the user mode networking status
    /// to retrieve the port mappings.
    ///
    /// The method returns if all the port mappings are made, or if there is an error communicating
    /// with QEMU. If emu_config().runtime.startup_timeout is positive, an error is returned if
    /// the mappings are not available within this time.
    async fn read_port_mappings(&mut self) -> Result<()> {
        // Check if there are any ports not already mapped.
        if !self.emu_config().host.port_map.values().any(|m| m.host.is_none()) {
            tracing::debug!("No unmapped ports found.");
            return Ok(());
        }

        let max_elapsed = if self.emu_config().runtime.startup_timeout.is_zero() {
            // if there is no timeout, we should technically return immediately, but it does
            // not make sense with unmapped ports, so give it a few seconds to try.
            Duration::from_secs(10)
        } else {
            self.emu_config().runtime.startup_timeout
        };

        // Open machine socket
        let instance_dir = &self.emu_config().runtime.instance_directory;
        let console_path = self.get_path_for_console_type(instance_dir, EngineConsoleType::Machine);
        let mut socket = QemuSocket::new(&console_path);

        // Start the timeout tracking here so it includes opening the socket,
        // which may have to wait for qemu to create the socket.
        let start = Instant::now();
        let mut qmp_stream = self.open_socket(&mut socket, &max_elapsed).await?;
        let mut response_iter =
            Deserializer::from_reader(qmp_stream.try_clone()?).into_iter::<Value>();

        // Loop reading the responses on the socket, and send the request to get the
        // user network information.
        loop {
            if start.elapsed() > max_elapsed {
                bail!("Reading port mappings timed out");
            }

            match response_iter.next() {
                Some(Ok(data)) => {
                    if let Some(return_string) = data.get("return") {
                        let port_pairs = Self::parse_return_string(
                            return_string.as_str().unwrap_or_else(|| ""),
                        )?;
                        let mut modified = false;
                        // Iterate over the parsed port pairs, then find the matching entry in
                        // the port map.
                        // There are a small number of ports that need to be mapped, so the
                        // nested loop should not be a performance concern.
                        for pair in port_pairs {
                            for v in self.emu_config_mut().host.port_map.values_mut() {
                                if v.guest == pair.guest {
                                    if v.host != Some(pair.host) {
                                        v.host = Some(pair.host);
                                        modified = true;
                                        tracing::info!("port mapped {pair:?}");
                                    }
                                }
                            }
                        }

                        // If the mapping was updated and there are no more unmapped ports,
                        // save and return.
                        if modified
                            && !self.emu_config().host.port_map.values().any(|m| m.host.is_none())
                        {
                            tracing::debug!("Writing updated mappings");
                            self.save_to_disk().await.context(
                                "Failed to write the emulation configuration file to disk.",
                            )?;
                            return Ok(());
                        }
                    } else {
                        tracing::debug!("Ignoring non return object {:?}", data);
                    }
                }
                Some(Err(e)) => {
                    tracing::debug!("Error reading qmp stream {e:?}")
                }
                None => {
                    tracing::debug!("None returned from qmp iterator");
                    // Pause a moment to allow qemu to make progress.
                    Timer::new(Duration::from_millis(100)).await;
                    continue;
                }
            };

            // Pause a moment to allow qemu to make progress.
            Timer::new(Duration::from_millis(100)).await;
            // Send { "execute": "human-monitor-command", "arguments": { "command-line": "info usernet" } }
            tracing::debug!("writing info usernet command");
            qmp_stream.write_fmt(format_args!(
                "{}\n",
                json!({
                    "execute": "human-monitor-command",
                    "arguments": { "command-line": "info usernet"}
                })
                .to_string()
            ))?;
        }
    }

    /// Parse the user network return string.
    /// The user network info is only available as text, so we need to parse the lines.
    /// This has been tested with AEMU and QEMU up to 7.0, but it is possible
    /// the format may change.
    fn parse_return_string(input: &str) -> Result<Vec<PortPair>> {
        let mut pairs: Vec<PortPair> = vec![];
        tracing::debug!("parsing_return_string return {input}");
        let mut saw_heading = false;
        for l in input.lines() {
            let parts: Vec<&str> = l.split_whitespace().map(|ele| ele.trim()).collect();

            // The heading has columns with multiple words, so the field count is more than the
            // data row.
            //Protocol[State]    FD  Source Address  Port   Dest. Address  Port RecvQ SendQ
            //TCP[ESTABLISHED]   63       10.0.2.15 56727  74.125.199.113   443     0     0
            match parts[..] {
                ["Protocol[State]", "FD", "Source", "Address", "Port", "Dest.", "Address", "Port", "RecvQ", "SendQ"] =>
                {
                    saw_heading = true;
                }
                [protocol_state, _, _, host_port, _, guest_port, _, _] => {
                    if protocol_state == "TCP[HOST_FORWARD]" {
                        let guest: u16 = guest_port.parse()?;
                        let host: u16 = host_port.parse()?;
                        pairs.push(PortPair { guest, host });
                    } else {
                        tracing::debug!("Skipping non host-forward row: {l}");
                    }
                }
                [] => tracing::debug!("Skipping empty line"),
                _ => tracing::debug!("Skipping unknown part collecton {parts:?}"),
            }
        }
        // Check that the heading column names have not changed. This could be a name change or schema change,
        // it could also be that the command did not return the header because the network objects are not available
        // yet, so log an error, but don't return an error.
        if !saw_heading {
            tracing::error!("Did not see expected header in {input}");
        }
        return Ok(pairs);
    }

    /// Opens the given socket waiting up to max_elapsed for the socket to be created and opened.
    async fn open_socket(
        &mut self,
        socket: &mut QemuSocket,
        max_elapsed: &Duration,
    ) -> Result<UnixStream> {
        let start = Instant::now();
        loop {
            if start.elapsed() > *max_elapsed {
                bail!("Reading port mappings timed out");
            }
            if !self.is_running().await {
                bail!("Emulator instance is not running.");
            }
            // Wait for being able to connect to the socket.
            match socket.connect().context("Connecting to console.") {
                Ok(()) => {
                    match socket.stream() {
                        Some(mut qmp_stream) => {
                            // Send the qmp_capabilities command to initialize the conversation.
                            qmp_stream.write_all(b"{ \"execute\": \"qmp_capabilities\" }\n")?;
                            return Ok(qmp_stream);
                        }
                        None => {
                            tracing::debug!("Could not open machine socket");
                        }
                    };
                }
                Err(e) => {
                    tracing::debug!("Could not open machine socket: {e:?}");
                }
            };

            Timer::new(Duration::from_millis(100)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use emulator_instance::{
        DataAmount, DataUnits, DiskImage, EmulatorInstanceData, EmulatorInstanceInfo, EngineType,
        PortMapping,
    };
    use ffx_config::{query, ConfigLevel};
    use serde::{Deserialize, Serialize};
    use std::{io::Read, os::unix::net::UnixListener};
    use tempfile::{tempdir, TempDir};

    #[derive(Default, Serialize)]
    struct TestEngine {}
    impl QemuBasedEngine for TestEngine {
        fn set_pid(&mut self, _pid: u32) {}
        fn get_pid(&self) -> u32 {
            todo!()
        }
        fn set_engine_state(&mut self, _state: EngineState) {}
        fn get_engine_state(&self) -> EngineState {
            todo!()
        }
    }
    #[async_trait]
    impl EmulatorEngine for TestEngine {
        fn engine_state(&self) -> EngineState {
            EngineState::default()
        }
        fn engine_type(&self) -> EngineType {
            EngineType::default()
        }
        async fn is_running(&mut self) -> bool {
            false
        }
    }
    const ORIGINAL: &str = "THIS_STRING";
    const ORIGINAL_PADDED: &str = "THIS_STRING\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
    const UPDATED: &str = "THAT_VALUE*";
    const UPDATED_PADDED: &str = "THAT_VALUE*\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

    #[derive(Copy, Clone)]
    enum DiskImageFormat {
        Fvm,
        Fxfs,
    }

    impl DiskImageFormat {
        pub fn as_disk_image(&self, path: impl AsRef<Path>) -> DiskImage {
            match self {
                Self::Fvm => DiskImage::Fvm(path.as_ref().to_path_buf()),
                Self::Fxfs => DiskImage::Fxfs(path.as_ref().to_path_buf()),
            }
        }
    }

    // Note that the caller MUST initialize the ffx_config environment before calling this function
    // since we override config values as part of the test. This looks like:
    //     let _env = ffx_config::test_init().await?;
    // The returned structure must remain in scope for the duration of the test to function
    // properly.
    async fn setup(
        guest: &mut GuestConfig,
        temp: &TempDir,
        disk_image_format: DiskImageFormat,
    ) -> Result<PathBuf> {
        let root = temp.path();

        let kernel_path = root.join("kernel");
        let zbi_path = root.join("zbi");
        let disk_image_path = disk_image_format.as_disk_image(root.join("disk"));

        let _ = fs::File::options()
            .write(true)
            .create(true)
            .open(&kernel_path)
            .context("cannot create test kernel file")?;
        let _ = fs::File::options()
            .write(true)
            .create(true)
            .open(&zbi_path)
            .context("cannot create test zbi file")?;
        let _ = fs::File::options()
            .write(true)
            .create(true)
            .open(&*disk_image_path)
            .context("cannot create test disk image file")?;

        query(config::EMU_INSTANCE_ROOT_DIR)
            .level(Some(ConfigLevel::User))
            .set(json!(root.display().to_string()))
            .await?;

        guest.kernel_image = kernel_path;
        guest.zbi_image = zbi_path;
        guest.disk_image = Some(disk_image_path);

        // Set the paths to use for the SSH keys
        query("ssh.pub")
            .level(Some(ConfigLevel::User))
            .set(json!([root.join("test_authorized_keys")]))
            .await?;
        query("ssh.priv")
            .level(Some(ConfigLevel::User))
            .set(json!([root.join("test_ed25519_key")]))
            .await?;

        Ok(PathBuf::from(root))
    }

    fn write_to(path: &PathBuf, value: &str) -> Result<()> {
        println!("Writing {} to {}", value, path.display());
        let mut file = File::options()
            .write(true)
            .open(path)
            .context(format!("cannot open existing file for write: {}", path.display()))?;
        File::write(&mut file, value.as_bytes())
            .context(format!("cannot write buffer to file: {}", path.display()))?;

        Ok(())
    }

    async fn test_staging_no_reuse_common(disk_image_format: DiskImageFormat) -> Result<()> {
        let _env = ffx_config::test_init().await?;
        let temp = tempdir().context("cannot get tempdir")?;
        let instance_name = "test-instance";
        let mut emu_config = EmulatorConfiguration::default();
        emu_config.device.storage = DataAmount { quantity: 32, units: DataUnits::Bytes };

        let root = setup(&mut emu_config.guest, &temp, disk_image_format).await?;

        let ctx = mock_modules::get_host_tool_context();
        ctx.expect().returning(|_| Ok(PathBuf::from("echo")));

        write_to(&emu_config.guest.kernel_image, ORIGINAL)
            .context("cannot write original value to kernel file")?;
        write_to(emu_config.guest.disk_image.as_ref().unwrap(), ORIGINAL)
            .context("cannot write original value to disk image file")?;

        let updated =
            <TestEngine as QemuBasedEngine>::stage_image_files(instance_name, &emu_config, false)
                .await;

        assert!(updated.is_ok(), "expected OK got {:?}", updated.unwrap_err());

        let actual = updated.context("cannot get updated guest config")?;
        let expected = GuestConfig {
            kernel_image: root.join(instance_name).join("kernel"),
            zbi_image: root.join(instance_name).join("zbi"),
            disk_image: Some(
                disk_image_format.as_disk_image(root.join(instance_name).join("disk")),
            ),
        };
        assert_eq!(actual, expected);

        // Test no reuse when old files exist. The original files should be overwritten.
        write_to(&emu_config.guest.kernel_image, UPDATED)
            .context("cannot write updated value to kernel file")?;
        write_to(emu_config.guest.disk_image.as_ref().unwrap(), UPDATED)
            .context("cannot write updated value to disk image file")?;

        let updated =
            <TestEngine as QemuBasedEngine>::stage_image_files(instance_name, &emu_config, false)
                .await;

        assert!(updated.is_ok(), "expected OK got {:?}", updated.unwrap_err());

        let actual = updated.context("cannot get updated guest config, reuse")?;
        let expected = GuestConfig {
            kernel_image: root.join(instance_name).join("kernel"),
            zbi_image: root.join(instance_name).join("zbi"),
            disk_image: Some(
                disk_image_format.as_disk_image(root.join(instance_name).join("disk")),
            ),
        };
        assert_eq!(actual, expected);

        println!("Reading contents from {}", actual.kernel_image.display());
        println!("Reading contents from {}", actual.disk_image.as_ref().unwrap().display());
        let mut kernel = File::open(&actual.kernel_image)
            .context("cannot open overwritten kernel file for read")?;
        let mut disk_image = File::open(&*actual.disk_image.unwrap())
            .context("cannot open overwritten disk image file for read")?;

        let mut kernel_contents = String::new();
        let mut fvm_contents = String::new();

        kernel
            .read_to_string(&mut kernel_contents)
            .context("cannot read contents of reused kernel file")?;
        disk_image
            .read_to_string(&mut fvm_contents)
            .context("cannot read contents of reused disk image file")?;

        assert_eq!(kernel_contents, UPDATED);

        // Fxfs will have ORIGINAL padded with nulls for be 32 bytes.
        //(set in emu_config at the top of this method).
        match disk_image_format {
            DiskImageFormat::Fvm => assert_eq!(fvm_contents, UPDATED),
            DiskImageFormat::Fxfs => assert_eq!(fvm_contents, UPDATED_PADDED),
        };

        Ok(())
    }

    #[fuchsia::test]
    async fn test_staging_no_reuse_fvm() -> Result<()> {
        test_staging_no_reuse_common(DiskImageFormat::Fvm).await
    }

    #[fuchsia::test]
    async fn test_staging_no_reuse_fxfs() -> Result<()> {
        test_staging_no_reuse_common(DiskImageFormat::Fxfs).await
    }

    async fn test_staging_with_reuse_common(disk_image_format: DiskImageFormat) -> Result<()> {
        let _env = ffx_config::test_init().await?;
        let temp = tempdir().context("cannot get tempdir")?;
        let instance_name = "test-instance";
        let mut emu_config = EmulatorConfiguration::default();
        emu_config.device.storage = DataAmount { quantity: 32, units: DataUnits::Bytes };

        let root = setup(&mut emu_config.guest, &temp, disk_image_format).await?;

        let ctx = mock_modules::get_host_tool_context();
        ctx.expect().returning(|_| Ok(PathBuf::from("echo")));

        // This checks if --reuse is true, but the directory isn't there to reuse; should succeed.
        write_to(&emu_config.guest.kernel_image, ORIGINAL)
            .context("cannot write original value to kernel file")?;
        write_to(emu_config.guest.disk_image.as_ref().unwrap(), ORIGINAL)
            .context("cannot write original value to disk image file")?;

        let updated: Result<GuestConfig> =
            <TestEngine as QemuBasedEngine>::stage_image_files(instance_name, &emu_config, true)
                .await;

        assert!(updated.is_ok(), "expected OK got {:?}", updated.unwrap_err());

        let actual = updated.context("cannot get updated guest config")?;
        let expected = GuestConfig {
            kernel_image: root.join(instance_name).join("kernel"),
            zbi_image: root.join(instance_name).join("zbi"),
            disk_image: Some(
                disk_image_format.as_disk_image(root.join(instance_name).join("disk")),
            ),
        };
        assert_eq!(actual, expected);

        // Test reuse. Note that the ZBI file isn't actually copied in the test, since we replace
        // the ZBI tool with an "echo" command.
        write_to(&emu_config.guest.kernel_image, UPDATED)
            .context("cannot write updated value to kernel file")?;
        write_to(emu_config.guest.disk_image.as_ref().unwrap(), UPDATED)
            .context("cannot write updated value to disk image file")?;

        let updated =
            <TestEngine as QemuBasedEngine>::stage_image_files(instance_name, &emu_config, true)
                .await;

        assert!(updated.is_ok(), "expected OK got {:?}", updated.unwrap_err());

        let actual = updated.context("cannot get updated guest config, reuse")?;
        let expected = GuestConfig {
            kernel_image: root.join(instance_name).join("kernel"),
            zbi_image: root.join(instance_name).join("zbi"),
            disk_image: Some(
                disk_image_format.as_disk_image(root.join(instance_name).join("disk")),
            ),
        };
        assert_eq!(actual, expected);

        println!("Reading contents from {}", actual.kernel_image.display());
        let mut kernel =
            File::open(&actual.kernel_image).context("cannot open reused kernel file for read")?;
        let mut fvm = File::open(&*actual.disk_image.unwrap())
            .context("cannot open reused fvm file for read")?;

        let mut kernel_contents = String::new();
        let mut fvm_contents = String::new();

        kernel
            .read_to_string(&mut kernel_contents)
            .context("cannot read contents of reused kernel file")?;
        fvm.read_to_string(&mut fvm_contents).context("cannot read contents of reused fvm file")?;

        assert_eq!(kernel_contents, ORIGINAL);

        // Fxfs will have ORIGINAL padded with nulls for be 32 bytes.
        //(set in emu_config at the top of this method).
        match disk_image_format {
            DiskImageFormat::Fvm => assert_eq!(fvm_contents, ORIGINAL),
            DiskImageFormat::Fxfs => assert_eq!(fvm_contents, ORIGINAL_PADDED),
        };

        Ok(())
    }

    #[fuchsia::test]
    async fn test_staging_with_reuse_fvm() -> Result<()> {
        test_staging_with_reuse_common(DiskImageFormat::Fvm).await
    }

    #[fuchsia::test]
    async fn test_staging_with_reuse_fxfs() -> Result<()> {
        test_staging_with_reuse_common(DiskImageFormat::Fxfs).await
    }

    // There's no equivalent test for FVM for now -- extending FVM images is more complex and
    // depends on an external binary, making testing challenging.
    #[fuchsia::test]

    async fn test_staging_resize_fxfs() -> Result<()> {
        let _env = ffx_config::test_init().await?;
        let temp = tempdir().context("cannot get tempdir")?;
        let instance_name = "test-instance";
        let mut emu_config = EmulatorConfiguration::default();
        let root = setup(&mut emu_config.guest, &temp, DiskImageFormat::Fxfs).await?;

        let ctx = mock_modules::get_host_tool_context();
        ctx.expect().returning(|_| Ok(PathBuf::from("echo")));

        const EXPECTED_DATA: &[u8] = b"hello, world";

        std::fs::write(&emu_config.guest.kernel_image, "whatever")?;
        std::fs::write(emu_config.guest.disk_image.as_ref().unwrap(), EXPECTED_DATA)?;

        emu_config.device.storage = DataAmount { units: DataUnits::Kilobytes, quantity: 4 };

        let config =
            <TestEngine as QemuBasedEngine>::stage_image_files(instance_name, &emu_config, false)
                .await
                .context("Failed to get guest config")?;

        let expected = GuestConfig {
            kernel_image: root.join(instance_name).join("kernel"),
            zbi_image: root.join(instance_name).join("zbi"),
            disk_image: Some(DiskImage::Fxfs(root.join(instance_name).join("disk"))),
        };
        assert_eq!(config, expected);

        let mut disk_image = File::open(&*config.disk_image.unwrap())?;
        let mut disk_contents = vec![];
        disk_image
            .read_to_end(&mut disk_contents)
            .context("cannot read contents of reused disk image file")?;

        assert_eq!(disk_contents.len(), 4096);
        assert_eq!(&disk_contents[..EXPECTED_DATA.len()], EXPECTED_DATA);
        assert_eq!(&disk_contents[EXPECTED_DATA.len()..], &[0u8; 4096 - EXPECTED_DATA.len()]);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_embed_boot_data() -> Result<()> {
        let _env = ffx_config::test_init().await?;
        let temp = tempdir().context("cannot get tempdir")?;
        let mut emu_config = EmulatorConfiguration::default();

        let root = setup(&mut emu_config.guest, &temp, DiskImageFormat::Fvm).await?;

        let ctx = mock_modules::get_host_tool_context();
        ctx.expect().returning(|_| Ok(PathBuf::from("echo")));

        let src = emu_config.guest.zbi_image;
        let dest = root.join("dest.zbi");

        <TestEngine as QemuBasedEngine>::embed_boot_data(&src, &dest).await?;

        Ok(())
    }

    #[test]
    fn test_validate_net() -> Result<()> {
        // User mode doesn't have specific requirements, so it should return OK.
        let engine = TestEngine::default();
        let mut emu_config = EmulatorConfiguration::default();
        emu_config.host.networking = NetworkingMode::User;
        let result = engine.validate_network_flags(&emu_config);
        assert!(result.is_ok(), "{:?}", result.unwrap_err());

        // No networking returns an error if no console is selected.
        emu_config.host.networking = NetworkingMode::None;
        emu_config.runtime.console = ConsoleType::None;
        let result = engine.validate_network_flags(&emu_config);
        assert!(result.is_err());

        emu_config.runtime.console = ConsoleType::Console;
        let result = engine.validate_network_flags(&emu_config);
        assert!(result.is_ok(), "{:?}", result.unwrap_err());

        emu_config.runtime.console = ConsoleType::Monitor;
        let result = engine.validate_network_flags(&emu_config);
        assert!(result.is_ok(), "{:?}", result.unwrap_err());

        // Tap mode errors if host is Linux and there's no interface, but we can't mock the
        // interface, so we can't test this case yet.
        emu_config.host.networking = NetworkingMode::Tap;

        // Validation runs after configuration is merged with values from PBMs and runtime, so Auto
        // values should already be resolved. If not, that's a failure.
        emu_config.host.networking = NetworkingMode::Auto;
        let result = engine.validate_network_flags(&emu_config);
        assert!(result.is_err());

        Ok(())
    }

    #[derive(Deserialize, Debug)]
    struct Arguments {
        #[serde(alias = "command-line")]
        pub command_line: String,
    }
    #[derive(Deserialize, Debug)]
    struct QMPCommand {
        pub execute: String,
        pub arguments: Option<Arguments>,
    }

    #[fuchsia::test]
    async fn test_read_port_mappings() -> Result<()> {
        let _env = ffx_config::test_init().await?;
        let temp = tempdir().context("cannot get tempdir")?;
        let mut data: EmulatorInstanceData =
            EmulatorInstanceData::new_with_state("test-instance", EngineState::New);
        let root =
            setup(&mut data.get_emulator_configuration_mut().guest, &temp, DiskImageFormat::Fvm)
                .await?;
        data.set_instance_directory(&root.join("test-instance").to_string_lossy());
        fs::create_dir_all(&data.get_emulator_configuration().runtime.instance_directory)?;

        data.get_emulator_configuration_mut()
            .host
            .port_map
            .insert("ssh".into(), PortMapping { guest: 22, host: None });
        data.get_emulator_configuration_mut()
            .host
            .port_map
            .insert("http".into(), PortMapping { guest: 80, host: None });
        data.get_emulator_configuration_mut()
            .host
            .port_map
            .insert("premapped".into(), PortMapping { guest: 11, host: Some(1111) });

        // use the current pid for the emulator instance

        let mut engine = crate::FemuEngine::new(data);
        engine.set_pid(std::process::id());
        engine.set_engine_state(EngineState::Running);

        // Change the working directory to handle long path names to the socket while opening it,
        // then change back.
        let cwd = env::current_dir()?;
        // Set up a socket that behaves like QMP
        env::set_current_dir(engine.emu_config().runtime.instance_directory.clone())?;
        let listener = UnixListener::bind(MACHINE_CONSOLE)?;
        env::set_current_dir(&cwd)?;

        // Helper function for this test to be the QEMU side of the QMP socket.
        fn do_qmp(mut stream: UnixStream) -> Result<()> {
            let mut request_iter =
                Deserializer::from_reader(stream.try_clone()?).into_iter::<Value>();

            // Responses to the `info usernet` request. The last response should end the interaction
            // because if fulfills all the port mappings which are being looked for.
            let responses = vec![
                json!({}),
                json!({"return" :
                 "VLAN -1 (net0):\r\nProtocol[State]    FD  Source Address  Port   Dest. Address  Port RecvQ SendQ\r\n"
                }),
                json!({"return": r#"VLAN -1 (net0):
                Protocol[State]    FD  Source Address  Port   Dest. Address  Port RecvQ SendQ
                TCP[HOST_FORWARD]  24               * 36167       10.0.2.15    22     0     0
                UDP[236 sec]       49               * 33338         0.0.0.0 33337     0     0
                "#}),
                json!({"return": r#"VLAN -1 (net0):
                Protocol[State]    FD  Source Address  Port   Dest. Address  Port RecvQ SendQ
                TCP[ESTABLISHED]   45       127.0.0.1 36167       10.0.2.15    22     0     0
                TCP[HOST_FORWARD]  25               * 36975       10.0.2.15    80     0     0
                TCP[HOST_FORWARD]  24               * 36167       10.0.2.15    22     0     0
                UDP[236 sec]       49               * 33338         0.0.0.0 33337     0     0
                "#}),
            ];

            let mut index = 0;
            loop {
                match request_iter.next() {
                    Some(Ok(data)) => {
                        if let Ok(cmd) = serde_json::from_value::<QMPCommand>(data.clone()) {
                            match cmd.execute.as_str() {
                                "human-monitor-command" => {
                                    if let Some(arguments) = cmd.arguments {
                                        assert_eq!(arguments.command_line, "info usernet");
                                    }
                                    stream.write_fmt(format_args!(
                                        "{}\n",
                                        responses[index].to_string()
                                    ))?;
                                    index += 1;
                                }
                                "qmp_capabilities" => {
                                    stream.write_all(
                                        json!(
                                        {
                                        "QMP": {
                                            "version": {
                                                "qemu": {
                                                    "micro": 0,
                                                    "minor": 12,
                                                    "major": 2
                                                },
                                                "package": "(gradle_1.3.0-beta4-78860-g2764d93fd1)"
                                                },
                                                "capabilities": []
                                                }
                                            }
                                        )
                                        .to_string()
                                        .as_bytes(),
                                    )?;
                                }
                                _ => bail!("unknown request is here! {cmd:?}"),
                            }
                        } else {
                            bail!("Unknown message {data:?}");
                        }
                    }
                    Some(Err(e)) => bail!("Error reading QMP request: {e:?}"),
                    None => (),
                }
            }
        }

        // Set up a side thread that will accept an incoming connection and then exit.
        let _listener_thread = std::thread::spawn(move || -> Result<()> {
            // accept connections and process them, spawning a new thread for each one
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        /* connection succeeded */
                        std::thread::spawn(|| match do_qmp(stream) {
                            Ok(_) => (),
                            Err(e) => panic!("do_qmp failed: {e:?}"),
                        });
                    }
                    Err(err) => {
                        /* connection failed */
                        bail!("{err:?}");
                    }
                }
            }
            Ok(())
        });

        <crate::FemuEngine as QemuBasedEngine>::read_port_mappings(&mut engine).await?;

        for (name, mapping) in &engine.emu_config().host.port_map {
            match name.as_str() {
                "http" => assert_eq!(
                    mapping.host,
                    Some(36975),
                    "mismatch for {:?}",
                    &engine.emu_config().host.port_map
                ),
                "ssh" => assert_eq!(
                    mapping.host,
                    Some(36167),
                    "mismatch for {:?}",
                    &engine.emu_config().host.port_map
                ),
                "premapped" => assert_eq!(
                    mapping.host,
                    Some(1111),
                    "mismatch for {:?}",
                    &engine.emu_config().host.port_map
                ),
                _ => bail!("Unexpected port mapping: {name} {mapping:?}"),
            };
        }

        Ok(())
    }

    #[test]
    fn test_parse_return_string() -> Result<()> {
        let normal_expected = r#"VLAN -1 (net0):\r
          Protocol[State]    FD  Source Address  Port   Dest. Address  Port RecvQ SendQ\r
          TCP[HOST_FORWARD]  81               * 43265       10.0.2.15  2345     0     0\r
          TCP[HOST_FORWARD]  80               * 38989       10.0.2.15  5353     0     0\r
          TCP[HOST_FORWARD]  79               * 43751       10.0.2.15    22     0     0\r"#;
        let condensed_expected = r#"VLAN -1 (net0):
          Protocol[State] FD Source Address Port  Dest. Address Port RecvQ SendQ
          TCP[HOST_FORWARD] 81    * 43265  10.0.2.15    2345 0 0
          TCP[HOST_FORWARD] 80 * 38989       10.0.2.15  5353     0     0\r
          TCP[HOST_FORWARD] 79   * 43751 10.0.2.15 22     0     0"#;
        let broken_expected = r#"VLAN -1 (net0):\r
          Protocol[State]    FD  Source Address  Port   Dest. Address  Port RecvQ SendQ\r
          TCP[HOST_FORWARD]  81               * 43265       10.0.2.15  2345     0     0\r
          TCP[HOST_FORWARD]  80               \r"#;
        let missing_fd_expected = r#"VLAN -1 (net0):\r
          Protocol[State]    FD  Source Address  Port   Dest. Address  Port RecvQ SendQ\r
          TCP[HOST_FORWARD]  81               * 43265       10.0.2.15  2345     0     0\r
          TCP[CLOSED]                         * 38989       10.0.2.15  5353     0     0\r
          TCP[SYN_SYNC]      80               * 43751       10.0.2.15    22     0     0\r"#;
        let established_expected = r#"VLAN -1 (net0):\r
          Protocol[State]    FD  Source Address  Port   Dest. Address  Port RecvQ SendQ\r
          TCP[ESTABLISHED]  81               * 42265       10.0.2.15  2345     0     0\r
          TCP[HOST_FORWARD]  83               * 43265       10.0.2.15  2345     0     0\r
          TCP[HOST_FORWARD]  80               * 38989       10.0.2.15  5353     0     0\r
          TCP[HOST_FORWARD]  79               * 43751       10.0.2.15    22     0     0\r"#;

        let testdata: Vec<(&str, Result<Vec<PortPair>>)> = vec![
            ("", Ok(vec![])),
            ("VLAN -1 (net0):\r\n  Protocol[State]    FD  Source Address  Port   Dest. Address  Port RecvQ SendQ\r\n", Ok(vec![])),
            (normal_expected, Ok(vec![
                PortPair{guest:2345, host:43265},
                PortPair{guest:5353, host:38989},
                PortPair{guest:22, host:43751}])),
            (condensed_expected, Ok(vec![
                    PortPair{guest:2345, host:43265},
                    PortPair{guest:5353, host:38989},
                    PortPair{guest:22, host:43751}])),
            (broken_expected, Ok(vec![
                        PortPair{guest:2345, host:43265}])),
            (missing_fd_expected, Ok(vec![
                            PortPair{guest:2345, host:43265}])),
            (established_expected, Ok(vec![
                PortPair{guest:2345, host:43265},
                PortPair{guest:5353, host:38989},
                PortPair{guest:22, host:43751}])),
        ];

        for (input, result) in testdata {
            let actual = <TestEngine as QemuBasedEngine>::parse_return_string(input);
            match actual {
                Ok(port_list) => assert_eq!(port_list, result.ok().unwrap()),
                Err(e) => assert_eq!(e.to_string(), result.err().unwrap().to_string()),
            };
        }
        Ok(())

        // TCP with other state.
    }
}
