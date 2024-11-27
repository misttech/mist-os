// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error, Result};
use async_trait::async_trait;
use chrono::prelude::*;
use chrono::LocalResult;
use fdio::service_connect;
use fidl::endpoints::create_proxy;
use fuchsia_async::{self as fasync, TimeoutExt};
use fuchsia_fs::directory;
use fuchsia_runtime::{UtcDuration, UtcInstant};
use futures::{select, StreamExt, TryFutureExt};
use std::path::PathBuf;
use std::pin::pin;
use thiserror::Error;
use tracing::{debug, error, warn};
use {fidl_fuchsia_hardware_rtc as frtc, fidl_fuchsia_io as fio};
#[cfg(test)]
use {fuchsia_sync::Mutex, std::sync::Arc};

static RTC_PATH: &str = "/dev/class/rtc";

/// Time to wait before declaring a FIDL call to be failed.
const FIDL_TIMEOUT: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(2);

// The minimum error at which to begin an async wait for top of second while setting RTC.
const WAIT_THRESHOLD: zx::MonotonicDuration = zx::MonotonicDuration::from_millis(1);

const RTC_DEVICE_OPEN_TIMEOUT: zx::MonotonicDuration = zx::MonotonicDuration::from_seconds(5);

const NANOS_PER_SECOND: i64 = 1_000_000_000;

#[derive(Error, Debug)]
pub enum RtcCreationError {
    #[error("Could not find any RTC devices")]
    NoDevices,
    #[error("Could not connect to RTC device: {0}")]
    ConnectionFailed(Error),
    #[error("Not configured to use RTC")]
    NotConfigured,
}

/// Interface to interact with a real-time clock. Note that the RTC hardware interface is limited
/// to a resolution of one second; times returned by the RTC will always be a whole number of
/// seconds and times sent to the RTC will discard any fractional second.
#[async_trait]
pub trait Rtc: Send + Sync {
    /// Returns the current time reported by the realtime clock.
    async fn get(&self) -> Result<UtcInstant>;
    /// Sets the time of the realtime clock to `value`.
    async fn set(&self, value: UtcInstant) -> Result<()>;
}

fn get_dir() -> Result<fio::DirectoryProxy, fuchsia_fs::node::OpenError> {
    directory::open_in_namespace(RTC_PATH, fio::PERM_READABLE)
}

/// An implementation of the `Rtc` trait that connects to an RTC device in /dev/class/rtc.
#[derive(Debug)]
pub struct RtcImpl {
    proxy: frtc::DeviceProxy,
}

impl RtcImpl {
    /// Returns a new `RtcImpl` connected to the only available RTC device. Returns an Error if no
    /// devices were found, multiple devices were found, or the connection failed.
    ///
    /// Args:
    /// - `has_rtc`: set to true if the board is configured with an RTC.
    pub async fn only_device(has_rtc: bool) -> Result<RtcImpl, RtcCreationError> {
        Self::only_device_for_test(has_rtc, get_dir).await
    }

    // Call directly only for tests.
    //
    // Args:
    // See `Self::only_device`.
    //
    // Generics:
    // - `F`: a closure that maybe provides a `DirectoryProxy` to be used as
    //   `/dev/class/rtc` directory.  Normally this is only set to non-default
    //   value in tests.
    async fn only_device_for_test<F>(
        has_rtc: bool,
        rtc_dir_source: F,
    ) -> Result<RtcImpl, RtcCreationError>
    where
        F: FnOnce() -> Result<fio::DirectoryProxy, fuchsia_fs::node::OpenError>,
    {
        debug!("has_rtc: {}", has_rtc);
        if has_rtc {
            let maybe_dir = rtc_dir_source();
            let dir = maybe_dir.map_err(|err| {
                RtcCreationError::ConnectionFailed(anyhow!(
                    "could not open {:?}: {}",
                    &*RTC_PATH,
                    err
                ))
            })?;

            let mut rtc_devices = device_watcher::watch_for_files(&dir)
                .await
                .map_err(|err| {
                    RtcCreationError::ConnectionFailed(anyhow!(
                        "could not watch {}: {}",
                        RTC_PATH,
                        err
                    ))
                })?
                .fuse();
            let mut timeout = pin!(fasync::Timer::new(RTC_DEVICE_OPEN_TIMEOUT));
            select! {
                device = rtc_devices.next() => {
                    match device {
                        Some(device) => {
                            let device = device.map_err(|err| {
                                RtcCreationError::ConnectionFailed(anyhow!(
                                    "could not read any RTC device from {}: {}",
                                    RTC_PATH,
                                    err
                                ))
                            })?;
                            fasync::Task::local(async move {
                                while let Some(device) = rtc_devices.next().await {
                                    // Attempt to alert to the existence of multiple RTC drivers here.
                                    // For the time being this situation probably means the choice
                                    // of the RTC to use is not stable over time.
                                    warn!("another RTC device appeared and was ignored: {:?}", device)
                                }
                            })
                            .detach();
                            let mut path = PathBuf::from(RTC_PATH);
                            path.push(device);
                            RtcImpl::new(path)
                        },
                        None => {
                            // While this should not happen in general, we may be better
                            // served by continuing without RTC if it does.
                            Err(RtcCreationError::NoDevices)
                        },
                    }
                },

                _ = timeout => {
                    Err(RtcCreationError::NoDevices)
                },
            }
        } else {
            debug!("no RTC was configured in {}", RTC_PATH);
            Err(RtcCreationError::NotConfigured)
        }
    }

    /// Returns a new `RtcImpl` connected to the device at the supplied path.
    pub fn new(path_buf: PathBuf) -> Result<RtcImpl, RtcCreationError> {
        let path_str = path_buf
            .to_str()
            .ok_or(RtcCreationError::ConnectionFailed(anyhow!("Non unicode path")))?;
        debug!("RTC at: {}", path_str);
        let (proxy, server) = create_proxy::<frtc::DeviceMarker>();
        service_connect(&path_str, server.into_channel()).map_err(|err| {
            RtcCreationError::ConnectionFailed(anyhow!("Failed to connect to device: {}", err))
        })?;
        Ok(RtcImpl { proxy })
    }
}

fn fidl_time_to_zx_time(fidl_time: frtc::Time) -> Result<UtcInstant> {
    let chrono = Utc.with_ymd_and_hms(
        fidl_time.year as i32,
        fidl_time.month as u32,
        fidl_time.day as u32,
        fidl_time.hours as u32,
        fidl_time.minutes as u32,
        fidl_time.seconds as u32,
    );
    match chrono {
        LocalResult::Single(t) => Ok(UtcInstant::from_nanos(t.timestamp_nanos_opt().unwrap())),
        _ => Err(anyhow!("Invalid RTC time: {:?}", fidl_time)),
    }
}

fn zx_time_to_fidl_time(zx_time: UtcInstant) -> frtc::Time {
    let nanos = UtcInstant::into_nanos(zx_time);
    let chrono = Utc.timestamp_opt(nanos / NANOS_PER_SECOND, 0).unwrap();
    frtc::Time {
        year: chrono.year() as u16,
        month: chrono.month() as u8,
        day: chrono.day() as u8,
        hours: chrono.hour() as u8,
        minutes: chrono.minute() as u8,
        seconds: chrono.second() as u8,
    }
}

#[async_trait]
impl Rtc for RtcImpl {
    async fn get(&self) -> Result<UtcInstant> {
        self.proxy
            .get()
            .map_err(|err| anyhow!("FIDL error on Rtc::get: {}", err))
            .on_timeout(zx::MonotonicInstant::after(FIDL_TIMEOUT), || {
                Err(anyhow!("FIDL timeout on Rtc::get"))
            })
            .await?
            .map_err(|err| anyhow!("Driver error on Rtc::get: {}", err))
            .and_then(fidl_time_to_zx_time)
    }

    async fn set(&self, value: UtcInstant) -> Result<()> {
        let fractional_second =
            zx::MonotonicDuration::from_nanos(value.into_nanos() % NANOS_PER_SECOND);
        // The RTC API only accepts integer seconds but we really need higher accuracy, particularly
        // for the kernel clock set by the RTC driver...
        let fidl_time = if fractional_second < WAIT_THRESHOLD {
            // ...if we are being asked to set a time at or near the bottom of the second, truncate
            // the time and set the RTC immediately...
            zx_time_to_fidl_time(value)
        } else {
            // ...otherwise, wait until the top of the current second than set the RTC using the
            // following second.
            fasync::Timer::new(fasync::MonotonicInstant::after(
                zx::MonotonicDuration::from_seconds(1) - fractional_second,
            ))
            .await;
            zx_time_to_fidl_time(value + UtcDuration::from_seconds(1))
        };
        let status = self
            .proxy
            .set(&fidl_time)
            .map_err(|err| anyhow!("FIDL error on Rtc::set: {}", err))
            .on_timeout(zx::MonotonicInstant::after(FIDL_TIMEOUT), || {
                Err(anyhow!("FIDL timeout on Rtc::set"))
            })
            .await?;
        zx::Status::ok(status).map_err(|stat| anyhow!("Bad status on Rtc::set: {:?}", stat))
    }
}

/// A Fake implementation of the Rtc trait for use in testing. The fake always returns a fixed
/// value set during construction and remembers the last value it was told to set (shared across
/// all clones of the `FakeRtc`).
#[cfg(test)]
#[derive(Clone)]
pub struct FakeRtc {
    /// The response used for get requests.
    value: Result<UtcInstant, String>,
    /// The most recent value received in a set request.
    last_set: Arc<Mutex<Option<UtcInstant>>>,
}

#[cfg(test)]
impl FakeRtc {
    /// Returns a new `FakeRtc` that always returns the supplied time.
    pub fn valid(time: UtcInstant) -> FakeRtc {
        FakeRtc { value: Ok(time), last_set: Arc::new(Mutex::new(None)) }
    }

    /// Returns a new `FakeRtc` that always returns the supplied error message.
    pub fn invalid(error: String) -> FakeRtc {
        FakeRtc { value: Err(error), last_set: Arc::new(Mutex::new(None)) }
    }

    /// Returns the last time set on this clock, or none if the clock has never been set.
    pub fn last_set(&self) -> Option<UtcInstant> {
        self.last_set.lock().map(|time| time.clone())
    }
}

#[cfg(test)]
#[async_trait]
impl Rtc for FakeRtc {
    async fn get(&self) -> Result<UtcInstant> {
        self.value.as_ref().map(|time| time.clone()).map_err(|msg| Error::msg(msg.clone()))
    }

    async fn set(&self, value: UtcInstant) -> Result<()> {
        let mut last_set = self.last_set.lock();
        last_set.replace(value);
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl::endpoints::create_proxy_and_stream;
    use fuchsia_async as fasync;
    use futures::StreamExt;
    use test_util::{assert_gt, assert_lt};

    const TEST_FIDL_TIME: frtc::Time =
        frtc::Time { year: 2020, month: 8, day: 14, hours: 0, minutes: 0, seconds: 0 };
    const INVALID_FIDL_TIME_1: frtc::Time =
        frtc::Time { year: 2020, month: 14, day: 0, hours: 0, minutes: 0, seconds: 0 };
    const INVALID_FIDL_TIME_2: frtc::Time =
        frtc::Time { year: 2020, month: 8, day: 14, hours: 99, minutes: 99, seconds: 99 };
    const TEST_OFFSET: UtcDuration = UtcDuration::from_millis(250);
    const TEST_ZX_TIME: UtcInstant = UtcInstant::from_nanos(1_597_363_200_000_000_000);
    const DIFFERENT_ZX_TIME: UtcInstant = UtcInstant::from_nanos(1_597_999_999_000_000_000);

    fn new_rw_rtc(proxy: frtc::DeviceProxy) -> RtcImpl {
        RtcImpl { proxy }
    }

    #[fuchsia::test]
    fn time_conversion() {
        let to_fidl = zx_time_to_fidl_time(TEST_ZX_TIME);
        assert_eq!(to_fidl, TEST_FIDL_TIME);
        // Times should be truncated to the previous second
        let to_fidl_2 = zx_time_to_fidl_time(TEST_ZX_TIME + UtcDuration::from_millis(999));
        assert_eq!(to_fidl_2, TEST_FIDL_TIME);

        let to_zx = fidl_time_to_zx_time(TEST_FIDL_TIME).unwrap();
        assert_eq!(to_zx, TEST_ZX_TIME);

        assert_eq!(fidl_time_to_zx_time(INVALID_FIDL_TIME_1).is_err(), true);
        assert_eq!(fidl_time_to_zx_time(INVALID_FIDL_TIME_2).is_err(), true);
    }

    #[fuchsia::test]
    async fn rtc_impl_get_valid() {
        let (proxy, mut stream) = create_proxy_and_stream::<frtc::DeviceMarker>().unwrap();

        let rtc_impl = new_rw_rtc(proxy);
        let _responder = fasync::Task::spawn(async move {
            if let Some(Ok(frtc::DeviceRequest::Get { responder })) = stream.next().await {
                responder.send(Ok(&TEST_FIDL_TIME)).expect("Failed response");
            }
        });
        assert_eq!(rtc_impl.get().await.unwrap(), TEST_ZX_TIME);
    }

    #[fuchsia::test]
    async fn rtc_impl_get_invalid() {
        let (proxy, mut stream) = create_proxy_and_stream::<frtc::DeviceMarker>().unwrap();

        let rtc_impl = new_rw_rtc(proxy);
        let _responder = fasync::Task::spawn(async move {
            if let Some(Ok(frtc::DeviceRequest::Get { responder })) = stream.next().await {
                responder.send(Ok(&INVALID_FIDL_TIME_1)).expect("Failed response");
            }
        });
        assert_eq!(rtc_impl.get().await.is_err(), true);
    }

    const RTC_SETUP_TIME: zx::MonotonicDuration = zx::MonotonicDuration::from_millis(90);

    #[fuchsia::test]
    async fn rtc_impl_set_whole_second() {
        let (proxy, mut stream) = create_proxy_and_stream::<frtc::DeviceMarker>().unwrap();

        let rtc_impl = new_rw_rtc(proxy);
        let _responder = fasync::Task::spawn(async move {
            if let Some(Ok(frtc::DeviceRequest::Set { rtc, responder })) = stream.next().await {
                let status = match rtc {
                    TEST_FIDL_TIME => zx::Status::OK,
                    _ => zx::Status::INVALID_ARGS,
                };
                responder.send(status.into_raw()).expect("Failed response");
            }
        });
        let before = zx::MonotonicInstant::get();
        assert!(rtc_impl.set(TEST_ZX_TIME).await.is_ok());
        let span = zx::MonotonicInstant::get() - before;
        // Setting an integer second should not require any delay and therefore should complete
        // very fast - well under a millisecond typically. We did observe ~54ms very rarely.
        assert_lt!(span, RTC_SETUP_TIME);
    }

    #[fuchsia::test]
    async fn rtc_impl_set_partial_second() {
        let (proxy, mut stream) = create_proxy_and_stream::<frtc::DeviceMarker>().unwrap();

        let rtc_impl = new_rw_rtc(proxy);
        let _responder = fasync::Task::spawn(async move {
            if let Some(Ok(frtc::DeviceRequest::Set { rtc, responder })) = stream.next().await {
                let status = match rtc {
                    TEST_FIDL_TIME => zx::Status::OK,
                    _ => zx::Status::INVALID_ARGS,
                };
                responder.send(status.into_raw()).expect("Failed response");
            }
        });
        let before = zx::MonotonicInstant::get();
        assert!(rtc_impl.set(TEST_ZX_TIME - TEST_OFFSET).await.is_ok());
        let span = zx::MonotonicInstant::get() - before;
        // Setting a fractional second should cause a delay until the top of second before calling
        // the FIDL interface. We only verify half the expected time has passed to allow for some
        // slack in the timer calculation.
        assert_gt!(span, zx::MonotonicDuration::from_nanos(TEST_OFFSET.into_nanos() / 2));
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn valid_fake() {
        let fake = FakeRtc::valid(TEST_ZX_TIME);
        assert_eq!(fake.get().await.unwrap(), TEST_ZX_TIME);
        assert_eq!(fake.last_set(), None);

        // Set a new time, this should be recorded but get should still return the original time.
        assert!(fake.set(DIFFERENT_ZX_TIME).await.is_ok());
        assert_eq!(fake.last_set(), Some(DIFFERENT_ZX_TIME));
        assert_eq!(fake.get().await.unwrap(), TEST_ZX_TIME);
    }

    #[fuchsia::test(allow_stalls = false)]
    async fn invalid_fake() {
        let message = "I'm designed to fail".to_string();
        let fake = FakeRtc::invalid(message.clone());
        assert_eq!(&fake.get().await.unwrap_err().to_string(), &message);
        assert_eq!(fake.last_set(), None);

        // Setting a new time should still succeed and be recorded but it won't make get valid.
        assert!(fake.set(DIFFERENT_ZX_TIME).await.is_ok());
        assert_eq!(fake.last_set(), Some(DIFFERENT_ZX_TIME));
        assert_eq!(&fake.get().await.unwrap_err().to_string(), &message);
    }

    use assert_matches::assert_matches;
    use vfs::directory::entry_container::Directory;
    use vfs::{execution_scope, pseudo_directory};

    fn serve_dir(root: Arc<impl Directory>) -> fio::DirectoryProxy {
        let (dir_proxy, dir_server) = fidl::endpoints::create_proxy::<fio::DirectoryMarker>();
        root.open(
            execution_scope::ExecutionScope::new(),
            fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::DIRECTORY,
            vfs::path::Path::dot().into(),
            fidl::endpoints::ServerEnd::new(dir_server.into_channel()),
        );
        dir_proxy
    }

    #[fuchsia::test]
    async fn no_rtc_configured() {
        let dir = pseudo_directory! {};
        let dir_proxy = serve_dir(dir);
        let dir_provider = || Ok(dir_proxy);
        let result = RtcImpl::only_device_for_test(/*has_rtc*/ false, dir_provider).await;
        assert_matches!(result, Err(RtcCreationError::NotConfigured))
    }

    #[fuchsia::test]
    async fn no_rtc_detected() {
        let dir = pseudo_directory! {};
        let dir_proxy = serve_dir(dir);
        let dir_provider = || Ok(dir_proxy);
        let result = RtcImpl::only_device_for_test(/*has_rtc*/ true, dir_provider).await;
        assert_matches!(result, Err(RtcCreationError::NoDevices))
    }

    #[fuchsia::test]
    async fn rtc_configured_and_detected() {
        let dir = pseudo_directory! {
                "deadbeef" => pseudo_directory! {
            },
        };
        let dir_proxy = serve_dir(dir);
        let dir_provider = || Ok(dir_proxy);
        let result = RtcImpl::only_device_for_test(/*has_rtc*/ true, dir_provider).await;
        // Connection fails because it's a dir, not a service, but we expected
        // that.
        assert_matches!(result, Err(RtcCreationError::ConnectionFailed(_)))
    }
}
