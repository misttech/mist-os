// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::{CapabilityProvider, CapabilitySource},
        model::{
            error::{CapabilityProviderError, ModelError},
            hooks::{Event, EventPayload, EventType, Hook, HooksRegistration},
        },
    },
    ::routing::capability_source::InternalCapability,
    async_trait::async_trait,
    cm_task_scope::TaskScope,
    cm_types::Name,
    cm_util::channel,
    elf_runner::process_launcher::ProcessLauncher,
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io as fio, fidl_fuchsia_process as fproc, fuchsia_zircon as zx,
    lazy_static::lazy_static,
    std::{
        path::PathBuf,
        sync::{Arc, Weak},
    },
    tracing::warn,
};

lazy_static! {
    pub static ref PROCESS_LAUNCHER_CAPABILITY_NAME: Name =
        "fuchsia.process.Launcher".parse().unwrap();
}

pub struct ProcessLauncherSvc();

impl ProcessLauncherSvc {
    pub fn new() -> Self {
        Self {}
    }

    pub fn hooks(self: &Arc<Self>) -> Vec<HooksRegistration> {
        vec![HooksRegistration::new(
            "ProcessLauncher",
            vec![EventType::CapabilityRouted],
            Arc::downgrade(self) as Weak<dyn Hook>,
        )]
    }

    async fn on_framework_capability_routed_async<'a>(
        self: Arc<Self>,
        capability: &'a InternalCapability,
        capability_provider: Option<Box<dyn CapabilityProvider>>,
    ) -> Result<Option<Box<dyn CapabilityProvider>>, ModelError> {
        if capability.matches_protocol(&PROCESS_LAUNCHER_CAPABILITY_NAME) {
            Ok(Some(
                Box::new(ProcessLauncherCapabilityProvider::new()) as Box<dyn CapabilityProvider>
            ))
        } else {
            Ok(capability_provider)
        }
    }
}

#[async_trait]
impl Hook for ProcessLauncherSvc {
    async fn on(self: Arc<Self>, event: &Event) -> Result<(), ModelError> {
        if let EventPayload::CapabilityRouted {
            source: CapabilitySource::Builtin { capability, .. },
            capability_provider,
        } = &event.payload
        {
            let mut capability_provider = capability_provider.lock().await;
            *capability_provider = self
                .on_framework_capability_routed_async(&capability, capability_provider.take())
                .await?;
        };
        Ok(())
    }
}

struct ProcessLauncherCapabilityProvider;

impl ProcessLauncherCapabilityProvider {
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl CapabilityProvider for ProcessLauncherCapabilityProvider {
    async fn open(
        self: Box<Self>,
        task_scope: TaskScope,
        _flags: fio::OpenFlags,
        _relative_path: PathBuf,
        server_end: &mut zx::Channel,
    ) -> Result<(), CapabilityProviderError> {
        let server_end = channel::take_channel(server_end);
        let server_end = ServerEnd::<fproc::LauncherMarker>::new(server_end);
        let stream: fproc::LauncherRequestStream =
            server_end.into_stream().map_err(|_| CapabilityProviderError::StreamCreationError)?;
        task_scope
            .add_task(async move {
                let result = ProcessLauncher::serve(stream).await;
                if let Err(error) = result {
                    warn!(%error, "ProcessLauncher.serve failed");
                }
            })
            .await;
        Ok(())
    }
}

// These tests are very similar to the tests in process_builder itself, and even reuse the test
// util from that, since the process_builder API is close to 1:1 with the process launcher service.
#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::model::hooks::Hooks,
        anyhow::{format_err, Context, Error},
        assert_matches::assert_matches,
        fidl::{
            endpoints::{ClientEnd, ServerEnd},
            prelude::*,
        },
        fidl_fuchsia_io as fio,
        fidl_test_processbuilder::{UtilMarker, UtilProxy},
        fuchsia_async as fasync,
        fuchsia_runtime::{job_default, HandleInfo, HandleType},
        fuchsia_zircon::HandleBased,
        futures::lock::Mutex,
        moniker::{AbsoluteMoniker, AbsoluteMonikerBase},
        std::{mem, sync::Weak},
        vfs::{
            directory::entry::DirectoryEntry, execution_scope::ExecutionScope,
            file::vmo::read_only, path, pseudo_directory,
        },
    };

    extern "C" {
        fn dl_clone_loader_service(handle: *mut zx::sys::zx_handle_t) -> zx::sys::zx_status_t;
    }

    // Clone the current loader service to provide to the new test processes.
    fn clone_loader_service() -> Result<zx::Handle, zx::Status> {
        let mut raw = 0;
        let status = unsafe { dl_clone_loader_service(&mut raw) };
        zx::Status::ok(status)?;

        let handle = unsafe { zx::Handle::from_raw(raw) };
        Ok(handle)
    }

    async fn serve_launcher(
    ) -> Result<(fproc::LauncherProxy, Arc<ProcessLauncherSvc>, TaskScope), Error> {
        let process_launcher = Arc::new(ProcessLauncherSvc::new());
        let hooks = Hooks::new();
        hooks.install(process_launcher.hooks()).await;

        let capability_provider = Arc::new(Mutex::new(None));
        let source = CapabilitySource::Builtin {
            capability: InternalCapability::Protocol(PROCESS_LAUNCHER_CAPABILITY_NAME.clone()),
            top_instance: Weak::new(),
        };

        let (client, mut server) = zx::Channel::create();

        let event = Event::new_for_test(
            AbsoluteMoniker::root(),
            "fuchsia-pkg://root",
            EventPayload::CapabilityRouted {
                source,
                capability_provider: capability_provider.clone(),
            },
        );
        hooks.dispatch(&event).await;

        let capability_provider = capability_provider.lock().await.take();
        let task_scope = TaskScope::new();
        if let Some(capability_provider) = capability_provider {
            capability_provider
                .open(task_scope.clone(), fio::OpenFlags::empty(), PathBuf::new(), &mut server)
                .await?;
        };

        let launcher_proxy = ClientEnd::<fproc::LauncherMarker>::new(client)
            .into_proxy()
            .expect("failed to create launcher proxy");
        Ok((launcher_proxy, process_launcher, task_scope))
    }

    fn connect_util(client: &zx::Channel) -> Result<UtilProxy, Error> {
        let (proxy, server) = zx::Channel::create();
        fdio::service_connect_at(&client, UtilMarker::PROTOCOL_NAME, server)
            .context("failed to connect to util service")?;
        Ok(UtilProxy::from_channel(fasync::Channel::from_channel(proxy)?))
    }

    fn check_process_running(process: &zx::Process) -> Result<(), Error> {
        let info = process.info()?;
        const STARTED: u32 = zx::ProcessInfoFlags::STARTED.bits();
        assert_matches!(
            info,
            zx::ProcessInfo {
                return_code: 0,
                start_time,
                flags: STARTED,
            } if start_time > 0
        );
        Ok(())
    }

    async fn check_process_exited_ok(process: &zx::Process) -> Result<(), Error> {
        fasync::OnSignals::new(process, zx::Signals::PROCESS_TERMINATED).await?;

        let info = process.info()?;
        const STARTED_AND_EXITED: u32 =
            zx::ProcessInfoFlags::STARTED.bits() | zx::ProcessInfoFlags::EXITED.bits();
        assert_matches!(
            info,
            zx::ProcessInfo {
                return_code: 0,
                start_time,
                flags: STARTED_AND_EXITED,
            } if start_time > 0
        );
        Ok(())
    }

    // Common setup for all tests that start a test util process through the launcher service.
    async fn setup_test_util(
        launcher: &fproc::LauncherProxy,
    ) -> Result<(fproc::LaunchInfo, UtilProxy), Error> {
        let file_proxy = fuchsia_fs::file::open_in_namespace(
            "/pkg/bin/process_builder_test_util",
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::RIGHT_EXECUTABLE,
        )?;
        let vmo = file_proxy
            .get_backing_memory(fio::VmoFlags::READ | fio::VmoFlags::EXECUTE)
            .await
            .map_err(|e| format_err!("getting test_util as exec failed: {}", e))?
            .map_err(zx::Status::from_raw)
            .map_err(|e| format_err!("getting test_util as exec failed: {}", e))?;
        let job = job_default();

        let (dir_client, dir_server) = zx::Channel::create();
        let handles = vec![
            fproc::HandleInfo {
                handle: dir_server.into_handle(),
                id: HandleInfo::new(HandleType::DirectoryRequest, 0).as_raw(),
            },
            fproc::HandleInfo {
                handle: clone_loader_service()?,
                id: HandleInfo::new(HandleType::LdsvcLoader, 0).as_raw(),
            },
        ];
        launcher.add_handles(handles)?;

        let launch_info = fproc::LaunchInfo {
            name: "process_builder_test_util".to_owned(),
            executable: vmo,
            job: job.duplicate(zx::Rights::SAME_RIGHTS)?,
        };
        let util_proxy = connect_util(&dir_client)?;
        Ok((launch_info, util_proxy))
    }

    #[fuchsia::test]
    async fn start_util_with_args() -> Result<(), Error> {
        let (launcher, _process_launcher, _task_scope) = serve_launcher().await?;
        let (launch_info, proxy) = setup_test_util(&launcher).await?;

        let test_args = &["arg0", "arg1", "arg2"];
        let test_args_bytes: Vec<_> = test_args.iter().map(|s| s.as_bytes().to_vec()).collect();
        launcher.add_args(&test_args_bytes)?;

        let (status, process) = launcher.launch(launch_info).await?;
        zx::Status::ok(status).context("Failed to launch test util process")?;
        let process = process.expect("Status was OK but no process returned");
        check_process_running(&process)?;

        let proc_args = proxy.get_arguments().await.context("failed to get args from util")?;
        assert_eq!(proc_args, test_args);

        mem::drop(proxy);
        check_process_exited_ok(&process).await?;
        Ok(())
    }

    #[fuchsia::test]
    async fn start_util_with_env() -> Result<(), Error> {
        let (launcher, _process_launcher, _task_scope) = serve_launcher().await?;
        let (launch_info, proxy) = setup_test_util(&launcher).await?;

        let test_env = &[("VAR1", "value2"), ("VAR2", "value2")];
        let test_env_strs: Vec<_> = test_env.iter().map(|v| format!("{}={}", v.0, v.1)).collect();
        let test_env_bytes: Vec<_> = test_env_strs.into_iter().map(|s| s.into_bytes()).collect();
        launcher.add_environs(&test_env_bytes)?;

        let (status, process) = launcher.launch(launch_info).await?;
        zx::Status::ok(status).context("Failed to launch test util process")?;
        let process = process.expect("Status was OK but no process returned");
        check_process_running(&process)?;

        let proc_env = proxy.get_environment().await.context("failed to get env from util")?;
        let proc_env_tuple: Vec<(&str, &str)> =
            proc_env.iter().map(|v| (&*v.key, &*v.value)).collect();
        assert_eq!(proc_env_tuple, test_env);
        Ok(())
    }

    #[fuchsia::test]
    async fn start_util_with_namespace_entries() -> Result<(), Error> {
        let (launcher, _process_launcher, _task_scope) = serve_launcher().await?;
        let (launch_info, proxy) = setup_test_util(&launcher).await?;

        let mut randbuf = [0; 8];
        zx::cprng_draw(&mut randbuf);
        let test_content = format!("test content {}", u64::from_le_bytes(randbuf));

        let test_content_bytes = test_content.clone().into_bytes();
        let (dir_server, dir_client) = zx::Channel::create();
        let dir = pseudo_directory! {
            "test_file" => read_only(test_content_bytes),
        };
        dir.clone().open(
            ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE
                | fio::OpenFlags::RIGHT_WRITABLE
                | fio::OpenFlags::DIRECTORY,
            path::Path::dot(),
            ServerEnd::new(dir_server),
        );

        let name_infos = vec![fproc::NameInfo {
            path: "/dir".to_string(),
            directory: ClientEnd::new(dir_client),
        }];
        launcher.add_names(name_infos)?;

        let (status, process) = launcher.launch(launch_info).await?;
        zx::Status::ok(status).context("Failed to launch test util process")?;
        let process = process.expect("Status was OK but no process returned");
        check_process_running(&process)?;

        let namespace_dump = proxy.dump_namespace().await.context("failed to dump namespace")?;
        assert_eq!(namespace_dump, "/dir, /dir/test_file");
        let dir_contents =
            proxy.read_file("/dir/test_file").await.context("failed to read file via util")?;
        assert_eq!(dir_contents, test_content);
        Ok(())
    }

    #[fuchsia::test]
    async fn create_without_starting() -> Result<(), Error> {
        let (launcher, _process_launcher, _task_scope) = serve_launcher().await?;
        let (launch_info, proxy) = setup_test_util(&launcher).await?;

        let test_args = &["arg0", "arg1", "arg2"];
        let test_args_bytes: Vec<_> = test_args.iter().map(|s| s.as_bytes().to_vec()).collect();
        launcher.add_args(&test_args_bytes)?;

        let (status, start_data) = launcher.create_without_starting(launch_info).await?;
        zx::Status::ok(status).context("Failed to launch test util process")?;
        let start_data = start_data.expect("Status was OK but no ProcessStartData returned");

        // Process should exist & be valid but not yet be running.
        let info = start_data.process.info()?;
        assert_eq!(info, zx::ProcessInfo { return_code: 0, start_time: 0, flags: 0 });

        // Start the process manually using the info from ProcessStartData.
        start_data
            .process
            .start(
                &start_data.thread,
                start_data.entry as usize,
                start_data.stack as usize,
                start_data.bootstrap.into_handle(),
                start_data.vdso_base as usize,
            )
            .context("Failed to start process from ProcessStartData")?;
        check_process_running(&start_data.process)?;

        let proc_args = proxy.get_arguments().await.context("failed to get args from util")?;
        assert_eq!(proc_args, test_args);

        mem::drop(proxy);
        check_process_exited_ok(&start_data.process).await?;
        Ok(())
    }
}
