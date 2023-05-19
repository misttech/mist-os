// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::hypervisor::Hypervisor,
    crate::virtual_machine::VirtualMachine,
    fidl::endpoints::{ControlHandle, RequestStream},
    fidl_fuchsia_virtualization::{
        GuestError, GuestLifecycleRequest, GuestLifecycleRequestStream, GuestLifecycleRunResponder,
        GuestRequest, GuestRequestStream,
    },
    fuchsia_zircon as zx,
    futures::{future::Fuse, select, stream::SelectAll, FutureExt, Stream, StreamExt},
    tracing,
};

pub enum OutgoingService {
    Guest(GuestRequestStream),
    GuestLifecycle(GuestLifecycleRequestStream),
}

impl From<GuestRequestStream> for OutgoingService {
    fn from(stream: GuestRequestStream) -> Self {
        OutgoingService::Guest(stream)
    }
}

impl From<GuestLifecycleRequestStream> for OutgoingService {
    fn from(stream: GuestLifecycleRequestStream) -> Self {
        OutgoingService::GuestLifecycle(stream)
    }
}

pub struct FidlServer<H: Hypervisor> {
    hypervisor: H,
    lifecycle_fidl: Option<GuestLifecycleRequestStream>,
    guest_fidl: SelectAll<GuestRequestStream>,
    virtual_machine: Option<VirtualMachine<H>>,
    run_responder: Option<GuestLifecycleRunResponder>,
}

impl<H: Hypervisor> FidlServer<H> {
    pub fn new(hypervisor: H) -> Self {
        Self {
            hypervisor,
            lifecycle_fidl: None,
            guest_fidl: SelectAll::new(),
            virtual_machine: None,
            run_responder: None,
        }
    }

    pub async fn run<St: Stream<Item = OutgoingService> + Unpin>(&mut self, outgoing: St) {
        let mut outgoing = outgoing.fuse();
        loop {
            let mut lifecycle_fut = match self.lifecycle_fidl.as_mut() {
                Some(stream) => stream.next().fuse(),
                None => Fuse::terminated(),
            };
            select! {
                stream = outgoing.next() => match stream {
                    Some(OutgoingService::GuestLifecycle(guest_lifecycle)) => {
                        // Allow only a single GuestLifecycle connection and drop any subsequent
                        // connections.
                        if self.lifecycle_fidl.is_none() {
                            self.lifecycle_fidl = Some(guest_lifecycle);
                        } else {
                            guest_lifecycle.control_handle().shutdown_with_epitaph(zx::Status::ALREADY_BOUND);
                        }
                    }
                    Some(OutgoingService::Guest(guest)) => {
                        if self.virtual_machine.is_some() {
                            self.guest_fidl.push(guest);
                        }
                    }
                    None => {
                        tracing::error!("Outgoing service stream closed");
                        return;
                    }
                },
                lifecycle_request = lifecycle_fut => {
                    match lifecycle_request {
                        Some(Ok(request)) => {
                            self.handle_lifecycle_request(request).await;
                        }
                        result => {
                            if let Some(Err(e)) = result {
                                tracing::warn!("FIDL Error reading GuestLifecycle channel: {}", e);
                            }
                            return;
                        }
                    }
                },
                guest_request = self.guest_fidl.next() => {
                    if let Some(Ok(request)) = guest_request {
                        self.handle_guest_request(request);
                    }
                },
            }
        }
    }

    async fn handle_lifecycle_request(&mut self, request: GuestLifecycleRequest) {
        match request {
            GuestLifecycleRequest::Create { guest_config, responder } => {
                if self.run_responder.is_some() {
                    if let Err(e) = responder.send(Err(GuestError::AlreadyRunning)) {
                        tracing::warn!(%e, "Failed to send GuestLifecycle.Create response");
                    }
                    return;
                }
                match VirtualMachine::new(self.hypervisor.clone(), guest_config) {
                    Err(e) => {
                        if let Err(e) = responder.send(Err(e)) {
                            tracing::warn!(%e, "Failed to send GuestLifecycle.Create response");
                        }
                    }
                    Ok(virtual_machine) => {
                        self.virtual_machine = Some(virtual_machine);
                        if let Err(e) = responder.send(Ok(())) {
                            tracing::warn!(%e, "Failed to send GuestLifecycle.Create response");
                        }
                    }
                }
            }
            GuestLifecycleRequest::Bind { guest, .. } => {
                if self.virtual_machine.is_some() {
                    match guest.into_stream() {
                        Ok(stream) => self.guest_fidl.push(stream),
                        Err(e) => {
                            tracing::warn!(%e, "Failed to create Guest RequestStream");
                        }
                    }
                }
            }
            GuestLifecycleRequest::Run { responder } => {
                if self.virtual_machine.is_none() {
                    if let Err(e) = responder.send(Err(GuestError::NotCreated)) {
                        tracing::warn!(%e, "Failed to send GuestLifecycle.Run response");
                    }
                    return;
                }
                if self.run_responder.is_some() {
                    if let Err(e) = responder.send(Err(GuestError::AlreadyRunning)) {
                        tracing::warn!(%e, "Failed to send GuestLifecycle.Run response");
                    }
                    return;
                }
                if let Err(e) = self.virtual_machine.as_mut().unwrap().start_primary_vcpu().await {
                    if let Err(e) = responder.send(Err(e)) {
                        tracing::warn!(%e, "Failed to send GuestLifecycle.Run response");
                    }
                    return;
                }
                self.run_responder = Some(responder);
            }
            GuestLifecycleRequest::Stop { responder } => {
                self.destroy_and_respond(Err(GuestError::ControllerForcedHalt));
                if let Err(e) = responder.send() {
                    tracing::warn!(%e, "Failed to send GuestLifecycle.Stop response");
                }
            }
        }
    }

    fn handle_guest_request(&mut self, request: GuestRequest) {
        match request {
            GuestRequest::GetConsole { .. } => {
                unimplemented!();
            }
            GuestRequest::GetSerial { .. } => {
                unimplemented!();
            }
            GuestRequest::GetHostVsockEndpoint { .. } => {
                unimplemented!();
            }
            GuestRequest::GetBalloonController { .. } => {
                unimplemented!();
            }
            GuestRequest::GetMemController { .. } => {
                unimplemented!();
            }
        }
    }

    fn destroy_and_respond(&mut self, result: Result<(), GuestError>) {
        self.guest_fidl.clear();
        self.virtual_machine = None;
        if let Some(responder) = self.run_responder.take() {
            responder.send(result).unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::hypervisor::testing::MockHypervisor,
        fidl::endpoints::Proxy,
        fidl_fuchsia_virtualization::{GuestConfig, GuestLifecycleMarker, GuestMarker},
        fuchsia_async as fasync,
        futures::{channel::mpsc, FutureExt},
    };

    fn build_valid_guest_config() -> GuestConfig {
        GuestConfig { guest_memory: Some(1 * 1024 * 1024 * 1024), ..Default::default() }
    }

    fn setup_test() -> (mpsc::Sender<OutgoingService>, fasync::Task<()>) {
        let (sender, receiver) = mpsc::channel::<OutgoingService>(1);
        let mut vmm = FidlServer::new(MockHypervisor::new());
        let run_task = fasync::Task::local(async move {
            vmm.run(receiver).await;
        });
        (sender, run_task)
    }

    fn connect_to_outgoing_service<T: fidl::endpoints::ProtocolMarker>(
        service_connector: &mut mpsc::Sender<OutgoingService>,
    ) -> T::Proxy
    where
        T: fidl::endpoints::ProtocolMarker,
        T::RequestStream: Into<OutgoingService>,
    {
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<T>().unwrap();
        service_connector.try_send(stream.into()).unwrap();
        proxy
    }

    #[fuchsia::test]
    async fn test_lifecycle_create() {
        let (mut service_connector, _task) = setup_test();

        let guest_lifecycle =
            connect_to_outgoing_service::<GuestLifecycleMarker>(&mut service_connector);

        assert_eq!(
            Ok(()),
            guest_lifecycle
                .create(build_valid_guest_config())
                .await
                .expect("FIDL Error creating VM")
        );
    }

    #[fuchsia::test]
    async fn test_lifecycle_create_after_run() {
        let (mut service_connector, task) = setup_test();

        let guest_lifecycle =
            connect_to_outgoing_service::<GuestLifecycleMarker>(&mut service_connector);

        assert_eq!(
            Ok(()),
            guest_lifecycle
                .create(build_valid_guest_config())
                .await
                .expect("FIDL Error creating VM")
        );
        let _ = guest_lifecycle.run();
        assert_eq!(
            Err(GuestError::AlreadyRunning),
            guest_lifecycle
                .create(build_valid_guest_config())
                .await
                .expect("FIDL Error creating VM")
        );

        // Drop the channel and await on the async task. Without this we could get crashes
        // because we need to join all threads (which are created when we run the VM). This
        // is because [task] owns the [FidlServer] and [VirtualMachine] instance so those
        // [Drop] impls are not running in scope of the test function body.
        //
        // TODO(https://fxbug.dev/102872): If we refactor this so that the [FidlServer] is
        // not owned by our exeuctor the cleanup will happen safely in the correct order.
        std::mem::drop(guest_lifecycle);
        task.await;
    }

    #[fuchsia::test]
    async fn test_lifecycle_run_stop() {
        let (mut service_connector, _task) = setup_test();

        let guest_lifecycle =
            connect_to_outgoing_service::<GuestLifecycleMarker>(&mut service_connector);

        assert_eq!(
            Ok(()),
            guest_lifecycle
                .create(build_valid_guest_config())
                .await
                .expect("FIDL Error creating VM")
        );

        // Call run twice. This is because run is a future that will not complete until we stop the
        // vm, so we don't want to await that yet. Instead we call run a second time and we use the
        // observation of the second run error to infer that the FIDL server has seen the first
        // run request.
        let mut run_fut1 = guest_lifecycle.run();
        let mut run_fut2 = guest_lifecycle.run();
        select! {
            result = run_fut1 => {
                panic!("GuestLifecycle.Run failed: {:?}", result);
            }
            result = run_fut2 => {
                assert_eq!(Err(GuestError::AlreadyRunning), result.expect("FIDL Error running VM"));
            }
        }

        // Now stop, this should cause run to return.
        guest_lifecycle.stop().await.expect("FIDL Error stopping guest");
        assert_eq!(
            Err(GuestError::ControllerForcedHalt),
            run_fut1
                .now_or_never()
                .expect("Run callback was not invoked after stopping VM")
                .expect("FIDL Error stopping VM")
        );
    }

    #[fuchsia::test]
    async fn test_lifecycle_run_not_created() {
        let (mut service_connector, _task) = setup_test();

        let guest_lifecycle =
            connect_to_outgoing_service::<GuestLifecycleMarker>(&mut service_connector);

        assert_eq!(
            Err(GuestError::NotCreated),
            guest_lifecycle.run().await.expect("FIDL Error running VM")
        );
    }

    #[fuchsia::test]
    async fn test_guest_connect_before_create() {
        let (mut service_connector, _task) = setup_test();

        let guest = connect_to_outgoing_service::<GuestMarker>(&mut service_connector);

        // Expect the guest channel to be closed because the guest hasn't been created yet using
        // GuestLifecycle.
        let result = guest.on_closed().await;
        assert_eq!(Ok(zx::Signals::CHANNEL_PEER_CLOSED), result);
    }

    #[fuchsia::test]
    async fn test_lifecycle_multiple_connections() {
        let (mut service_connector, _task) = setup_test();

        let lifecycle1 =
            connect_to_outgoing_service::<GuestLifecycleMarker>(&mut service_connector);
        let lifecycle2 =
            connect_to_outgoing_service::<GuestLifecycleMarker>(&mut service_connector);

        let signals = lifecycle2.on_closed().await.unwrap();
        // Expect the channel is closed and also readable since we expect it to contain an epitaph.
        assert!(signals.contains(zx::Signals::CHANNEL_PEER_CLOSED));
        assert!(signals.contains(zx::Signals::CHANNEL_READABLE));
        match lifecycle2.take_event_stream().next().await {
            Some(Err(fidl::Error::ClientChannelClosed { status, protocol_name })) => {
                assert_eq!(status, zx::Status::ALREADY_BOUND);
                assert_eq!(protocol_name, "fuchsia.virtualization.GuestLifecycle");
            }
            result => {
                panic!("Did not receive expected epitaph: {:?}", result);
            }
        }

        assert!(lifecycle1.on_closed().now_or_never().is_none());
    }

    #[fuchsia::test]
    async fn test_lifecycle_stop_on_disconnect() {
        let (mut service_connector, task) = setup_test();

        let guest_lifecycle =
            connect_to_outgoing_service::<GuestLifecycleMarker>(&mut service_connector);

        assert_eq!(
            Ok(()),
            guest_lifecycle
                .create(build_valid_guest_config())
                .await
                .expect("FIDL Error creating VM")
        );

        std::mem::drop(guest_lifecycle);

        // This will complete when FidlServer::run returns.
        task.await;
    }
}
