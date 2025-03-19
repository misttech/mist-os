// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fsync::Mutex;
use futures_util::StreamExt;
use std::sync::Arc;
use zx::MonotonicDuration;
use {fuchsia_async as fasync, fuchsia_sync as fsync};

/// Holds the memory stall rate increase of the system.
#[derive(Clone)]
pub struct MemoryStallRate {
    /// Measurement interval of the stall rate.
    pub interval: MonotonicDuration,
    /// Rate of memory stalls on some CPUs.
    pub rate_some: zx::sys::zx_duration_mono_t,
    /// Rate of memory stalls on all CPUs.
    pub rate_full: zx::sys::zx_duration_mono_t,
}

pub trait StallProviderTrait: Sync + Send {
    /// Return the current memory stall values from the kernel.
    fn get_stall_info(&self) -> Result<zx::MemoryStall, anyhow::Error>;
    /// Return the
    fn get_stall_rate(&self) -> Option<MemoryStallRate>;
}

pub struct StallProvider {
    /// Task that regularly polls the memory stall information from the kernel.
    _monitoring_task: fasync::Task<Result<(), anyhow::Error>>,
    /// Last values and rates of memory stalls computed.
    last_stall_info: Arc<Mutex<StallInformation>>,
    /// Memory stall kernel resource, for issuing queries.
    stall_resource: Arc<dyn StallResource>,
}

struct StallInformation {
    last_stall_values: zx::MemoryStall,
    stall_rate: Option<MemoryStallRate>,
}

/// Trait for a resource exposing memory stall information. Used for dependency injection in unit
/// tests.
pub trait StallResource: Sync + Send {
    fn get_memory_stall(&self) -> Result<zx::MemoryStall, zx::Status>;
}

impl StallResource for zx::Resource {
    fn get_memory_stall(&self) -> Result<zx::MemoryStall, zx::Status> {
        self.memory_stall()
    }
}

impl StallProvider {
    /// Create a new [StallProvider]. `stall_rate_interval` represents the polling delay between two
    /// memory stall measurements, and the interval for memory stall rate computation.
    pub fn new(
        stall_rate_interval: MonotonicDuration,
        stall_resource: Arc<dyn StallResource>,
    ) -> Result<StallProvider, anyhow::Error> {
        let last_stall_info = Arc::new(Mutex::new(StallInformation {
            last_stall_values: stall_resource.get_memory_stall()?,
            stall_rate: None,
        }));
        let stall_resource_clone = stall_resource.clone();
        let last_stall_clone = last_stall_info.clone();
        let monitoring_task = fasync::Task::spawn(async move {
            let mut interval_timer = fasync::Interval::new(stall_rate_interval);
            while let Some(()) = interval_timer.next().await {
                let new_stall_values = stall_resource.get_memory_stall()?;

                let mut stall_info = last_stall_clone.lock();
                stall_info.stall_rate = Some(MemoryStallRate {
                    interval: stall_rate_interval,
                    rate_some: new_stall_values.stall_time_some
                        - stall_info.last_stall_values.stall_time_some,
                    rate_full: new_stall_values.stall_time_full
                        - stall_info.last_stall_values.stall_time_full,
                });

                stall_info.last_stall_values = new_stall_values;
            }
            Ok(())
        });

        Ok(StallProvider {
            _monitoring_task: monitoring_task,
            last_stall_info,
            stall_resource: stall_resource_clone,
        })
    }
}

impl StallProviderTrait for StallProvider {
    fn get_stall_info(&self) -> Result<zx::MemoryStall, anyhow::Error> {
        Ok(self.stall_resource.get_memory_stall()?)
    }

    fn get_stall_rate(&self) -> Option<MemoryStallRate> {
        self.last_stall_info.lock().stall_rate.clone()
    }
}

#[cfg(test)]
mod tests {
    use fuchsia_async::MonotonicInstant;

    use super::*;
    use std::pin::pin;

    struct FakeStallResource {
        pub stall_value: Arc<Mutex<zx::MemoryStall>>,
    }

    impl StallResource for FakeStallResource {
        fn get_memory_stall(&self) -> Result<zx::MemoryStall, zx::Status> {
            Ok(self.stall_value.lock().clone())
        }
    }

    #[test]
    fn test_get_stall_rate() {
        let mut exec = fasync::TestExecutor::new_with_fake_time();
        exec.set_fake_time(MonotonicInstant::from_nanos(0));

        let stall_value =
            Arc::new(Mutex::new(zx::MemoryStall { stall_time_some: 0, stall_time_full: 0 }));

        stall_value.lock().stall_time_some = 1;
        stall_value.lock().stall_time_full = 2;

        let stall_provider = StallProvider::new(
            MonotonicDuration::from_minutes(1),
            Arc::new(FakeStallResource { stall_value: stall_value.clone() }),
        )
        .expect("Failed to create StallProvider");

        let current_stall_value = stall_provider.get_stall_info().expect("No stall info");
        assert_eq!(stall_value.lock().stall_time_some, current_stall_value.stall_time_some);
        assert_eq!(stall_value.lock().stall_time_full, current_stall_value.stall_time_full);
        assert!(stall_provider.get_stall_rate().is_none());

        assert!(exec
            .run_until_stalled(&mut pin!(async {
                fasync::TestExecutor::advance_to(MonotonicInstant::after(
                    MonotonicDuration::from_seconds(1),
                ))
                .await
            }))
            .is_ready());

        assert!(stall_provider.get_stall_rate().is_none());

        stall_value.lock().stall_time_some = 2;
        stall_value.lock().stall_time_full = 4;

        assert!(exec
            .run_until_stalled(&mut pin!(async {
                fasync::TestExecutor::advance_to(MonotonicInstant::after(
                    MonotonicDuration::from_seconds(60),
                ))
                .await
            }))
            .is_ready());

        let rate = stall_provider.get_stall_rate().expect("No stall rate");

        assert_eq!(1, rate.rate_some);
        assert_eq!(2, rate.rate_full);

        let current_stall_value = stall_provider.get_stall_info().expect("No stall info");
        assert_eq!(stall_value.lock().stall_time_some, current_stall_value.stall_time_some);
        assert_eq!(stall_value.lock().stall_time_full, current_stall_value.stall_time_full);
    }
}
