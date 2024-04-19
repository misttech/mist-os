// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use ffx_command::FfxContext;
use fidl::endpoints::create_proxy;
use fidl_fuchsia_hardware_audio as fhaudio;
use fidl_fuchsia_io as fio;

/// Connect to an instance of a FIDL protocol hosted in `directory` using the given `path`.
// This is essentially the same as `fuchsia_component::client::connect_to_named_protocol_at_dir_root`.
// We can't use the `fuchsia_component` library in ffx because it doesn't build on host.
fn connect_to_named_protocol_at_dir_root<P: fidl::endpoints::ProtocolMarker>(
    directory: &fio::DirectoryProxy,
    path: &str,
) -> fho::Result<P::Proxy> {
    let (proxy, server_end) = create_proxy::<P>().unwrap();
    directory
        .open(
            fio::OpenFlags::empty(),
            fio::ModeType::empty(),
            path,
            server_end.into_channel().into(),
        )
        .bug_context("Failed to call Directory.Open")?;
    Ok(proxy)
}

/// Connects to the `fuchsia.hardware.audio.Codec` protocol node in the `dev_class` directory
/// at `path`.
pub fn connect_hw_codec(
    dev_class: &fio::DirectoryProxy,
    path: &str,
) -> fho::Result<fhaudio::CodecProxy> {
    let connector_proxy =
        connect_to_named_protocol_at_dir_root::<fhaudio::CodecConnectorMarker>(dev_class, path)
            .bug_context("Failed to connect to CodecConnector")?;

    let (proxy, server_end) = create_proxy::<fhaudio::CodecMarker>().unwrap();
    connector_proxy.connect(server_end).bug_context("Failed to call Connect")?;

    Ok(proxy)
}

/// Connects to the `fuchsia.hardware.audio.Dai` protocol node in the `dev_class` directory
/// at `path`.
pub fn connect_hw_dai(
    dev_class: &fio::DirectoryProxy,
    path: &str,
) -> fho::Result<fhaudio::DaiProxy> {
    let connector_proxy =
        connect_to_named_protocol_at_dir_root::<fhaudio::DaiConnectorMarker>(dev_class, path)
            .bug_context("Failed to connect to DaiConnector")?;

    let (proxy, server_end) = create_proxy::<fhaudio::DaiMarker>().unwrap();
    connector_proxy.connect(server_end).bug_context("Failed to call Connect")?;

    Ok(proxy)
}

/// Connects to the `fuchsia.hardware.audio.Composite` protocol node in the `dev_class` directory
/// at `path`.
pub fn connect_hw_composite(
    dev_class: &fio::DirectoryProxy,
    path: &str,
) -> fho::Result<fhaudio::CompositeProxy> {
    // DFv2 Composite drivers do not use a connector/trampoline like Codec/Dai/StreamConfig.
    connect_to_named_protocol_at_dir_root::<fhaudio::CompositeMarker>(dev_class, path)
        .bug_context("Failed to connect to Composite")
}

/// Connects to the `fuchsia.hardware.audio.StreamConfig` protocol node in the `dev_class` directory
/// at `path`.
pub fn connect_hw_streamconfig(
    dev_class: &fio::DirectoryProxy,
    path: &str,
) -> fho::Result<fhaudio::StreamConfigProxy> {
    let connector_proxy = connect_to_named_protocol_at_dir_root::<
        fhaudio::StreamConfigConnectorMarker,
    >(dev_class, path)
    .bug_context("Failed to connect to StreamConfigConnector")?;

    let (proxy, server_end) = create_proxy::<fhaudio::StreamConfigMarker>().unwrap();
    connector_proxy.connect(server_end).bug_context("Failed to call Connect")?;

    Ok(proxy)
}
