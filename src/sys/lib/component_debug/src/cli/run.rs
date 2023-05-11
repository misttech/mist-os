// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        cli::format::{
            format_create_error, format_destroy_error, format_resolve_error, format_start_error,
        },
        lifecycle::{
            create_instance_in_collection, destroy_instance_in_collection, resolve_instance,
            start_instance, ActionError, CreateError, DestroyError,
        },
    },
    anyhow::{bail, format_err, Result},
    fidl::HandleBased,
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_process as fprocess,
    fidl_fuchsia_sys2 as fsys,
    fuchsia_url::AbsoluteComponentUrl,
    futures::AsyncReadExt,
    moniker::{
        AbsoluteMoniker, AbsoluteMonikerBase, ChildMonikerBase, RelativeMoniker,
        RelativeMonikerBase,
    },
    std::io::Read,
};

// This value is fairly arbitrary. The value matches `MAX_BUF` from `fuchsia.io`, but that
// constant is for `fuchsia.io.File` transfers, which are unrelated to these `zx::socket`
// transfers.
const TRANSFER_CHUNK_SIZE: usize = 8192;

async fn copy<W: std::io::Write>(source: fidl::Socket, mut sink: W) -> Result<()> {
    let mut source = fuchsia_async::Socket::from_socket(source)?;
    let mut buf = [0u8; TRANSFER_CHUNK_SIZE];
    loop {
        let bytes_read = source.read(&mut buf).await?;
        if bytes_read == 0 {
            return Ok(());
        }
        sink.write_all(&buf[..bytes_read])?;
        sink.flush()?;
    }
}

// The normal Rust representation of this constant is in fuchsia-runtime, which
// cannot be used on host. Maybe there's a way to move fruntime::HandleType and
// fruntime::HandleInfo to a place that can be used on host?
fn handle_id_for_fd(fd: u32) -> u32 {
    const PA_FD: u32 = 0x30;
    PA_FD | fd << 16
}

struct Stdio {
    local_in: fidl::Socket,
    local_out: fidl::Socket,
    local_err: fidl::Socket,
}

impl Stdio {
    fn new() -> (Self, Vec<fprocess::HandleInfo>) {
        let (local_in, remote_in) = fidl::Socket::create_stream();
        let (local_out, remote_out) = fidl::Socket::create_stream();
        let (local_err, remote_err) = fidl::Socket::create_stream();

        (
            Self { local_in, local_out, local_err },
            vec![
                fprocess::HandleInfo { handle: remote_in.into_handle(), id: handle_id_for_fd(0) },
                fprocess::HandleInfo { handle: remote_out.into_handle(), id: handle_id_for_fd(1) },
                fprocess::HandleInfo { handle: remote_err.into_handle(), id: handle_id_for_fd(2) },
            ],
        )
    }

    async fn forward(self) {
        let local_in = self.local_in;
        let local_out = self.local_out;
        let local_err = self.local_err;

        std::thread::spawn(move || {
            let mut term_in = std::io::stdin().lock();
            let mut buf = [0u8; TRANSFER_CHUNK_SIZE];
            loop {
                let bytes_read = term_in.read(&mut buf)?;
                if bytes_read == 0 {
                    return Ok::<(), anyhow::Error>(());
                }
                local_in.write(&buf[..bytes_read])?;
            }
        });

        std::thread::spawn(move || {
            let mut executor = fuchsia_async::LocalExecutor::new();
            let _result: Result<()> = executor
                .run_singlethreaded(async move { copy(local_err, std::io::stderr()).await });
        });

        std::thread::spawn(move || {
            let mut executor = fuchsia_async::LocalExecutor::new();
            let _result: Result<()> = executor
                .run_singlethreaded(async move { copy(local_out, std::io::stdout().lock()).await });
            std::process::exit(0);
        });

        // If we're following stdio, we just wait forever. When stdout is
        // closed, the whole process will exit.
        let () = futures::future::pending().await;
    }
}

pub async fn run_cmd<W: std::io::Write>(
    moniker: AbsoluteMoniker,
    url: AbsoluteComponentUrl,
    recreate: bool,
    connect_stdio: bool,
    lifecycle_controller: fsys::LifecycleControllerProxy,
    mut writer: W,
) -> Result<()> {
    let parent = moniker
        .parent()
        .ok_or(format_err!("Error: {} does not reference a dynamic instance", moniker))?;
    let leaf = moniker
        .leaf()
        .ok_or(format_err!("Error: {} does not reference a dynamic instance", moniker))?;
    let child_name = leaf.name();
    let collection = leaf
        .collection()
        .ok_or(format_err!("Error: {} does not reference a dynamic instance", moniker))?;

    // Convert the absolute moniker into a relative moniker w.r.t. root.
    // LifecycleController expects relative monikers only.
    let parent_relative = RelativeMoniker::scope_down(&AbsoluteMoniker::root(), &parent).unwrap();

    if recreate {
        // First try to destroy any existing instance at this monker.
        match destroy_instance_in_collection(
            &lifecycle_controller,
            &parent_relative,
            collection,
            child_name,
        )
        .await
        {
            Ok(()) => {
                writeln!(writer, "Destroyed existing component instance at {}...", moniker)?;
            }
            Err(DestroyError::ActionError(ActionError::InstanceNotFound)) => {
                // No component exists at this moniker. Nothing to do.
            }
            Err(e) => return Err(format_destroy_error(&moniker, e)),
        }
    }

    let (maybe_stdio, child_args) = if connect_stdio {
        let (stdio, numbered_handles) = Stdio::new();

        (
            Some(stdio),
            Some(fcomponent::CreateChildArgs {
                numbered_handles: Some(numbered_handles),
                ..Default::default()
            }),
        )
    } else {
        (None, None)
    };

    writeln!(writer, "URL: {}", url)?;
    writeln!(writer, "Moniker: {}", moniker)?;
    writeln!(writer, "Creating component instance...")?;

    let create_result = create_instance_in_collection(
        &lifecycle_controller,
        &parent_relative,
        collection,
        child_name,
        &url,
        child_args,
    )
    .await;

    match create_result {
        Err(CreateError::InstanceAlreadyExists) => {
            bail!("\nError: {} already exists.\nUse --recreate to destroy and create a new instance, or provide a different moniker.\n", moniker)
        }
        Err(e) => {
            return Err(format_create_error(&moniker, &parent, collection, e));
        }
        Ok(()) => {}
    }

    let child_relative = RelativeMoniker::scope_down(&AbsoluteMoniker::root(), &moniker).unwrap();
    writeln!(writer, "Resolving component instance...")?;
    resolve_instance(&lifecycle_controller, &child_relative)
        .await
        .map_err(|e| format_resolve_error(&moniker, e))?;

    writeln!(writer, "Starting component instance...")?;
    start_instance(&lifecycle_controller, &child_relative)
        .await
        .map_err(|e| format_start_error(&moniker, e))?;

    if let Some(stdio) = maybe_stdio {
        stdio.forward().await;
    }

    writeln!(writer, "Ran component instance!")?;

    Ok(())
}

#[cfg(test)]
mod test {
    use {
        super::*, anyhow::Result, fidl::endpoints::create_proxy_and_stream,
        fidl_fuchsia_sys2 as fsys, futures::TryStreamExt,
    };

    fn setup_fake_lifecycle_controller_ok(
        expected_parent_moniker: &'static str,
        expected_collection: &'static str,
        expected_name: &'static str,
        expected_url: &'static str,
        expected_moniker: &'static str,
        expect_destroy: bool,
    ) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            if expect_destroy {
                let req = stream.try_next().await.unwrap().unwrap();
                match req {
                    fsys::LifecycleControllerRequest::DestroyInstance {
                        parent_moniker,
                        child,
                        responder,
                    } => {
                        assert_eq!(expected_parent_moniker, parent_moniker);
                        assert_eq!(expected_name, child.name);
                        assert_eq!(expected_collection, child.collection.unwrap());
                        responder.send(&mut Ok(())).unwrap();
                    }
                    _ => panic!("Unexpected Lifecycle Controller request: {:?}", req),
                }
            }

            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::CreateInstance {
                    parent_moniker,
                    collection,
                    decl,
                    responder,
                    args: _,
                } => {
                    assert_eq!(expected_parent_moniker, parent_moniker);
                    assert_eq!(expected_collection, collection.name);
                    assert_eq!(expected_name, decl.name.unwrap());
                    assert_eq!(expected_url, decl.url.unwrap());
                    responder.send(&mut Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request: {:?}", req),
            }

            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::ResolveInstance { moniker, responder } => {
                    assert_eq!(expected_moniker, moniker);
                    responder.send(&mut Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request: {:?}", req),
            }

            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::StartInstance {
                    moniker,
                    binder: _,
                    responder,
                } => {
                    assert_eq!(expected_moniker, moniker);
                    responder.send(&mut Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request: {:?}", req),
            }
        })
        .detach();
        lifecycle_controller
    }

    fn setup_fake_lifecycle_controller_fail(
        expected_parent_moniker: &'static str,
        expected_collection: &'static str,
        expected_name: &'static str,
        expected_url: &'static str,
    ) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::DestroyInstance {
                    parent_moniker,
                    child,
                    responder,
                } => {
                    assert_eq!(expected_parent_moniker, parent_moniker);
                    assert_eq!(expected_name, child.name);
                    assert_eq!(expected_collection, child.collection.unwrap());
                    responder.send(&mut Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request: {:?}", req),
            }

            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::CreateInstance {
                    parent_moniker,
                    collection,
                    decl,
                    responder,
                    args: _,
                } => {
                    assert_eq!(expected_parent_moniker, parent_moniker);
                    assert_eq!(expected_collection, collection.name);
                    assert_eq!(expected_name, decl.name.unwrap());
                    assert_eq!(expected_url, decl.url.unwrap());
                    responder.send(&mut Err(fsys::CreateError::InstanceAlreadyExists)).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request: {:?}", req),
            }
        })
        .detach();
        lifecycle_controller
    }

    fn setup_fake_lifecycle_controller_recreate(
        expected_parent_moniker: &'static str,
        expected_collection: &'static str,
        expected_name: &'static str,
        expected_url: &'static str,
        expected_moniker: &'static str,
    ) -> fsys::LifecycleControllerProxy {
        let (lifecycle_controller, mut stream) =
            create_proxy_and_stream::<fsys::LifecycleControllerMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::DestroyInstance {
                    parent_moniker,
                    child,
                    responder,
                } => {
                    assert_eq!(expected_parent_moniker, parent_moniker);
                    assert_eq!(expected_name, child.name);
                    assert_eq!(expected_collection, child.collection.unwrap());
                    responder.send(&mut Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request: {:?}", req),
            }

            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::CreateInstance {
                    parent_moniker,
                    collection,
                    decl,
                    responder,
                    args: _,
                } => {
                    assert_eq!(expected_parent_moniker, parent_moniker);
                    assert_eq!(expected_collection, collection.name);
                    assert_eq!(expected_name, decl.name.unwrap());
                    assert_eq!(expected_url, decl.url.unwrap());
                    responder.send(&mut Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request: {:?}", req),
            }

            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::ResolveInstance { moniker, responder } => {
                    assert_eq!(expected_moniker, moniker);
                    responder.send(&mut Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request: {:?}", req),
            }

            let req = stream.try_next().await.unwrap().unwrap();
            match req {
                fsys::LifecycleControllerRequest::StartInstance {
                    moniker,
                    binder: _,
                    responder,
                } => {
                    assert_eq!(expected_moniker, moniker);
                    responder.send(&mut Ok(())).unwrap();
                }
                _ => panic!("Unexpected Lifecycle Controller request: {:?}", req),
            }
        })
        .detach();
        lifecycle_controller
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_ok() -> Result<()> {
        let mut output = Vec::new();
        let lifecycle_controller = setup_fake_lifecycle_controller_ok(
            "./some",
            "collection",
            "name",
            "fuchsia-pkg://fuchsia.com/test#meta/test.cm",
            "./some/collection:name",
            true,
        );
        let response = run_cmd(
            "/some/collection:name".try_into().unwrap(),
            "fuchsia-pkg://fuchsia.com/test#meta/test.cm".try_into().unwrap(),
            true,
            false,
            lifecycle_controller,
            &mut output,
        )
        .await;
        response.unwrap();
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_name() -> Result<()> {
        let mut output = Vec::new();
        let lifecycle_controller = setup_fake_lifecycle_controller_ok(
            "./core",
            "ffx-laboratory",
            "foobar",
            "fuchsia-pkg://fuchsia.com/test#meta/test.cm",
            "./core/ffx-laboratory:foobar",
            false,
        );
        let response = run_cmd(
            "/core/ffx-laboratory:foobar".try_into().unwrap(),
            "fuchsia-pkg://fuchsia.com/test#meta/test.cm".try_into().unwrap(),
            false,
            false,
            lifecycle_controller,
            &mut output,
        )
        .await;
        response.unwrap();
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_fail() -> Result<()> {
        let mut output = Vec::new();
        let lifecycle_controller = setup_fake_lifecycle_controller_fail(
            "./core",
            "ffx-laboratory",
            "test",
            "fuchsia-pkg://fuchsia.com/test#meta/test.cm",
        );
        let response = run_cmd(
            "/core/ffx-laboratory:test".try_into().unwrap(),
            "fuchsia-pkg://fuchsia.com/test#meta/test.cm".try_into().unwrap(),
            true,
            false,
            lifecycle_controller,
            &mut output,
        )
        .await;
        response.unwrap_err();
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_recreate() -> Result<()> {
        let mut output = Vec::new();
        let lifecycle_controller = setup_fake_lifecycle_controller_recreate(
            "./core",
            "ffx-laboratory",
            "test",
            "fuchsia-pkg://fuchsia.com/test#meta/test.cm",
            "./core/ffx-laboratory:test",
        );
        let response = run_cmd(
            "/core/ffx-laboratory:test".try_into().unwrap(),
            "fuchsia-pkg://fuchsia.com/test#meta/test.cm".try_into().unwrap(),
            true,
            false,
            lifecycle_controller,
            &mut output,
        )
        .await;
        response.unwrap();
        Ok(())
    }
}
