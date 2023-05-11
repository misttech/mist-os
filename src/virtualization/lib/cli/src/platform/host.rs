// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::platform::PlatformServices,
    anyhow::Result,
    async_trait::async_trait,
    fidl::endpoints::{create_proxy, DiscoverableProtocolMarker},
    fidl_fuchsia_developer_remotecontrol::RemoteControlProxy,
    fidl_fuchsia_io::OpenFlags,
    fidl_fuchsia_virtualization::{
        GuestManagerMarker, GuestManagerProxy, LinuxManagerMarker, LinuxManagerProxy,
    },
    guest_cli_args::GuestType,
};

pub struct HostPlatformServices {
    remote_control: RemoteControlProxy,
}

impl HostPlatformServices {
    pub fn new(remote_control: RemoteControlProxy) -> Self {
        Self { remote_control }
    }
}

#[async_trait(?Send)]
impl PlatformServices for HostPlatformServices {
    async fn connect_to_manager(&self, guest_type: GuestType) -> Result<GuestManagerProxy> {
        let (guest_manager, server_end) = create_proxy::<GuestManagerMarker>()?;
        // This may fail, but we report the error when we later try to use the GuestManagerProxy.
        let _ = self
            .remote_control
            .connect_capability(
                guest_type.moniker(),
                guest_type.guest_manager_interface(),
                server_end.into_channel(),
                OpenFlags::RIGHT_READABLE,
            )
            .await?;
        Ok(guest_manager)
    }

    async fn connect_to_linux_manager(&self) -> Result<LinuxManagerProxy> {
        let (linux_manager, server_end) = create_proxy::<LinuxManagerMarker>()?;
        // This may fail, but we report the error when we later try to use the LinuxManagerProxy.
        let _ = self
            .remote_control
            .connect_capability(
                GuestType::Termina.moniker(),
                LinuxManagerMarker::PROTOCOL_NAME,
                server_end.into_channel(),
                OpenFlags::RIGHT_READABLE,
            )
            .await?;
        Ok(linux_manager)
    }
}
