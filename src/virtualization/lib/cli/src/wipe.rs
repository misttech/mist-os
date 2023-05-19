// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::platform::PlatformServices, anyhow::Error,
    fidl_fuchsia_virtualization::LinuxManagerProxy, fuchsia_zircon_status as zx_status,
    guest_cli_args as arguments, std::fmt,
};

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum WipeResult {
    WipeCompleted,
    IncorrectGuestState,
    WipeFailure(i32),
    UnsupportedGuest(arguments::GuestType),
}

impl fmt::Display for WipeResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WipeResult::WipeCompleted => write!(f, "Successfully wiped guest"),
            WipeResult::WipeFailure(status) => {
                write!(f, "Failed to wipe data: {}", zx_status::Status::from_raw(*status))
            }
            WipeResult::IncorrectGuestState => {
                write!(
                    f,
                    concat!(
                        "The VM has already started. Please stop the guest ",
                        "(by restarting the host or issuing a guest stop command) and retry."
                    )
                )
            }
            WipeResult::UnsupportedGuest(guest) => {
                write!(f, "Wipe is not supported for '{}'. Only 'termina' is supported", guest)
            }
        }
    }
}

pub async fn handle_wipe<P: PlatformServices>(
    services: &P,
    args: &arguments::wipe_args::WipeArgs,
) -> Result<WipeResult, Error> {
    if args.guest_type != arguments::GuestType::Termina {
        return Ok(WipeResult::UnsupportedGuest(args.guest_type));
    }

    let linux_manager = services.connect_to_linux_manager().await?;
    do_wipe(linux_manager).await
}

async fn do_wipe(proxy: LinuxManagerProxy) -> Result<WipeResult, Error> {
    let result = match proxy.wipe_data().await?.map_err(zx_status::Status::from_raw) {
        Err(zx_status::Status::BAD_STATE) => WipeResult::IncorrectGuestState,
        Err(status) => WipeResult::WipeFailure(status.into_raw()),
        Ok(()) => WipeResult::WipeCompleted,
    };

    Ok(result)
}

#[cfg(test)]
mod test {
    use {
        super::*, crate::platform::FuchsiaPlatformServices,
        fidl::endpoints::create_proxy_and_stream, fidl_fuchsia_virtualization::LinuxManagerMarker,
        fuchsia_async as fasync, futures::StreamExt,
    };

    fn serve_mock_manager(response: zx_status::Status) -> LinuxManagerProxy {
        let (proxy, mut stream) = create_proxy_and_stream::<LinuxManagerMarker>()
            .expect("failed to create LinuxManager proxy/stream");
        fasync::Task::local(async move {
            let responder = stream
                .next()
                .await
                .expect("mock manager expected a request")
                .unwrap()
                .into_wipe_data()
                .expect("unexpected call to mock manager");

            if response == zx_status::Status::OK {
                responder.send(Ok(())).expect("failed to send mock response");
            } else {
                responder.send(Err(response.into_raw())).expect("failed to send mock response");
            }
        })
        .detach();

        proxy
    }

    #[fasync::run_until_stalled(test)]
    async fn unsupported_guest_type() {
        let services = FuchsiaPlatformServices::new();
        let result = handle_wipe(
            &services,
            &arguments::wipe_args::WipeArgs { guest_type: arguments::GuestType::Debian },
        )
        .await
        .unwrap();

        assert_eq!(result, WipeResult::UnsupportedGuest(arguments::GuestType::Debian));
    }

    #[fasync::run_until_stalled(test)]
    async fn incorrect_guest_state() {
        let proxy = serve_mock_manager(zx_status::Status::BAD_STATE);
        let result = do_wipe(proxy).await.unwrap();
        assert_eq!(result, WipeResult::IncorrectGuestState);
    }

    #[fasync::run_until_stalled(test)]
    async fn guest_wipe_failure() {
        let proxy = serve_mock_manager(zx_status::Status::NOT_FOUND);
        let result = do_wipe(proxy).await.unwrap();
        assert_eq!(result, WipeResult::WipeFailure(zx_status::Status::NOT_FOUND.into_raw()));
    }

    #[fasync::run_until_stalled(test)]
    async fn guest_successfully_wiped() {
        let proxy = serve_mock_manager(zx_status::Status::OK);
        let result = do_wipe(proxy).await.unwrap();
        assert_eq!(result, WipeResult::WipeCompleted);
    }
}
