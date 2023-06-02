// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{bail, Context, Error},
    fidl::endpoints::{ClientEnd, DiscoverableProtocolMarker, RequestStream},
    fidl_fuchsia_fs::{AdminMarker, AdminRequest, AdminRequestStream},
    fidl_fuchsia_fs_startup::{
        CheckOptions, FormatOptions, StartOptions, StartupMarker, StartupRequest,
        StartupRequestStream,
    },
    fidl_fuchsia_hardware_block::BlockMarker,
    fidl_fuchsia_io as fio,
    fidl_fuchsia_process_lifecycle::{LifecycleRequest, LifecycleRequestStream},
    fuchsia_async as fasync,
    fuchsia_fatfs::{fatfs_error_to_status, FatDirectory, FatFs},
    fuchsia_zircon as zx,
    futures::{lock::Mutex, TryStreamExt},
    remote_block_device::RemoteBlockClientSync,
    std::sync::Arc,
    tracing::{error, info, warn},
    vfs::{
        directory::{entry::DirectoryEntry, helper::DirectlyMutable},
        execution_scope::ExecutionScope,
        node::Node as _,
        path::Path,
    },
};

fn map_to_raw_status(e: Error) -> zx::sys::zx_status_t {
    map_to_status(e).into_raw()
}

fn map_to_status(error: Error) -> zx::Status {
    match error.downcast::<zx::Status>() {
        Ok(status) => status,
        Err(error) => match error.downcast::<std::io::Error>() {
            Ok(io_error) => fatfs_error_to_status(io_error),
            Err(error) => {
                // Print the internal error if we re-map it because we will lose any context after
                // this.
                warn!(?error, "Internal error");
                zx::Status::INTERNAL
            }
        },
    }
}

enum State {
    ComponentStarted,
    Running(RunningState),
}

struct RunningState {
    // We have to wrap this in an Arc, even though it itself basically just wraps an Arc, so that
    // FsInspectTree can reference `fs` as a Weak<dyn FsInspect>`.
    fs: FatFs,
}

impl State {
    async fn stop(&mut self, outgoing_dir: &vfs::directory::immutable::Simple) {
        if let State::Running(RunningState { fs }) =
            std::mem::replace(self, State::ComponentStarted)
        {
            info!("Stopping fatfs runtime; remaining connections will be forcibly closed");

            if let Ok(Some(entry)) = outgoing_dir
                .remove_entry_impl("root".to_string(), /* must_be_directory: */ false)
            {
                let _ = entry.into_any().downcast::<FatDirectory>().unwrap().close();
            }

            fs.shut_down().unwrap_or_else(|e| error!("Failed to shutdown fatfs: {:?}", e));
        }
    }
}

pub struct Component {
    state: Mutex<State>,

    // The execution scope of the pseudo filesystem.
    scope: ExecutionScope,

    // The root of the pseudo filesystem for the component.
    outgoing_dir: Arc<vfs::directory::immutable::Simple>,
}

impl Component {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::ComponentStarted),
            scope: ExecutionScope::new(),
            outgoing_dir: vfs::directory::immutable::simple(),
        })
    }

    /// Runs Fatfs as a component.
    pub async fn run(
        self: Arc<Self>,
        outgoing_dir: zx::Channel,
        lifecycle_channel: Option<zx::Channel>,
    ) -> Result<(), Error> {
        let svc_dir = vfs::directory::immutable::simple();
        self.outgoing_dir.add_entry("svc", svc_dir.clone()).expect("Unable to create svc dir");
        let weak = Arc::downgrade(&self);
        svc_dir.add_entry(
            StartupMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let weak = weak.clone();
                async move {
                    if let Some(me) = weak.upgrade() {
                        let _ = me.handle_startup_requests(requests).await;
                    }
                }
            }),
        )?;

        let weak = Arc::downgrade(&self);
        svc_dir.add_entry(
            AdminMarker::PROTOCOL_NAME,
            vfs::service::host(move |requests| {
                let weak = weak.clone();
                async move {
                    if let Some(me) = weak.upgrade() {
                        let _ = me.handle_admin_requests(requests).await;
                    }
                }
            }),
        )?;

        self.outgoing_dir.clone().open(
            self.scope.clone(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_WRITABLE,
            Path::dot(),
            outgoing_dir.into(),
        );

        if let Some(channel) = lifecycle_channel {
            let me = self.clone();
            self.scope.spawn(async move {
                if let Err(error) = me.handle_lifecycle_requests(channel).await {
                    warn!(?error, "handle_lifecycle_requests");
                }
            });
        }

        self.scope.wait().await;

        Ok(())
    }

    async fn handle_startup_requests(&self, mut stream: StartupRequestStream) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await? {
            match request {
                StartupRequest::Start { responder, device, options } => {
                    responder.send(self.handle_start(device, options).await.map_err(|e| {
                        error!(?e, "handle_start failed");
                        map_to_raw_status(e)
                    }))?
                }
                StartupRequest::Format { responder, device, options } => {
                    responder.send(self.handle_format(device, options).await.map_err(|e| {
                        error!(?e, "handle_format failed");
                        map_to_raw_status(e)
                    }))?
                }
                StartupRequest::Check { responder, device, options } => {
                    responder.send(self.handle_check(device, options).await.map_err(|e| {
                        error!(?e, "handle_check failed");
                        map_to_raw_status(e)
                    }))?
                }
            }
        }
        Ok(())
    }

    async fn handle_start(
        &self,
        device: ClientEnd<BlockMarker>,
        options: StartOptions,
    ) -> Result<(), Error> {
        info!(?options, "Received start request");

        let mut state = self.state.lock().await;
        state.stop(&self.outgoing_dir).await;

        let remote_block_client = RemoteBlockClientSync::new(device)?;
        let device = remote_block_device::Cache::new(remote_block_client)?;

        // Start the filesystem and open the root directory.
        let fs = FatFs::new(Box::new(device)).map_err(|_| zx::Status::IO)?;
        let root = fs.get_root()?;

        self.outgoing_dir.add_entry("root", root)?;

        *state = State::Running(RunningState { fs });

        info!("Mounted");
        Ok(())
    }

    async fn handle_format(
        &self,
        device: ClientEnd<BlockMarker>,
        options: FormatOptions,
    ) -> Result<(), Error> {
        let args: Box<dyn Iterator<Item = _> + Send> =
            if let Some(spc) = options.sectors_per_cluster {
                Box::new(["-c".to_string(), format!("{spc}")].into_iter())
            } else {
                Box::new(std::iter::empty())
            };
        if block_adapter::run(device.into_proxy()?, "/pkg/bin/mkfs-msdosfs", args).await? == 0 {
            Ok(())
        } else {
            bail!(zx::Status::IO)
        }
    }

    async fn handle_check(
        &self,
        device: ClientEnd<BlockMarker>,
        _options: CheckOptions,
    ) -> Result<(), Error> {
        // Pass the '-n' flag so that it never modifies which remains consistent with other
        // filesystems.
        if block_adapter::run(
            device.into_proxy()?,
            "/pkg/bin/fsck-msdosfs",
            ["-n".to_string()].into_iter(),
        )
        .await?
            == 0
        {
            Ok(())
        } else {
            bail!(zx::Status::IO)
        }
    }

    async fn handle_admin_requests(&self, mut stream: AdminRequestStream) -> Result<(), Error> {
        while let Some(request) = stream.try_next().await.context("Reading request")? {
            if self.handle_admin(request).await? {
                break;
            }
        }
        Ok(())
    }

    // Returns true if we should close the connection.
    async fn handle_admin(&self, req: AdminRequest) -> Result<bool, Error> {
        match req {
            AdminRequest::Shutdown { responder } => {
                info!("Received shutdown request");
                self.shutdown().await;
                responder
                    .send()
                    .unwrap_or_else(|e| warn!("Failed to send shutdown response: {}", e));
                return Ok(true);
            }
        }
    }

    async fn shutdown(&self) {
        self.state.lock().await.stop(&self.outgoing_dir).await;
        info!("Filesystem terminated");
    }

    async fn handle_lifecycle_requests(&self, lifecycle_channel: zx::Channel) -> Result<(), Error> {
        let mut stream =
            LifecycleRequestStream::from_channel(fasync::Channel::from_channel(lifecycle_channel)?);
        match stream.try_next().await.context("Reading request")? {
            Some(LifecycleRequest::Stop { .. }) => {
                info!("Received Lifecycle::Stop request");
                self.shutdown().await;
            }
            None => {}
        }
        Ok(())
    }
}
