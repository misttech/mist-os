// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::sysfs::{try_get, SysfsError, SysfsOps};
use crate::{sysfs_errno, sysfs_error};
use starnix_logging::log_error;
use starnix_uapi::errno;
use std::time::Duration;
use {fidl_fuchsia_hardware_google_nanohub as fnanohub, zx};

#[derive(Default)]
pub struct FirmwareNameSysFsOps {}

impl SysfsOps<fnanohub::DeviceSynchronousProxy> for FirmwareNameSysFsOps {
    fn show(&self, service: &fnanohub::DeviceSynchronousProxy) -> Result<String, SysfsError> {
        Ok(service.get_firmware_name(zx::MonotonicInstant::INFINITE)?)
    }
}

#[derive(Default)]
pub struct FirmwareVersionSysFsOps {}

impl SysfsOps<fnanohub::DeviceSynchronousProxy> for FirmwareVersionSysFsOps {
    fn show(&self, service: &fnanohub::DeviceSynchronousProxy) -> Result<String, SysfsError> {
        let v = service.get_firmware_version(zx::MonotonicInstant::INFINITE)?;
        Ok(format!(
            "hw type: {:04x} hw ver: {:04x} bl ver: {:04x} os ver: {:04x} variant ver: {:08x}\n",
            v.hardware_type,
            v.hardware_version,
            v.bootloader_version,
            v.os_version,
            v.variant_version
        ))
    }
}

#[derive(Default)]
pub struct TimeSyncSysFsOps {}

impl SysfsOps<fnanohub::DeviceSynchronousProxy> for TimeSyncSysFsOps {
    fn show(
        &self,
        service: &fidl_fuchsia_hardware_google_nanohub::DeviceSynchronousProxy,
    ) -> Result<String, SysfsError> {
        let response = service.get_time_sync(zx::MonotonicInstant::INFINITE)??;
        let ap = try_get(response.ap_boot_time)?;
        let mcu = try_get(response.mcu_boot_time)?;
        Ok(format!("{ap} {mcu}\n"))
    }
}

#[derive(Default)]
pub struct WakeLockSysFsOps {}

impl SysfsOps<fnanohub::DeviceSynchronousProxy> for WakeLockSysFsOps {
    fn store(
        &self,
        service: &fidl_fuchsia_hardware_google_nanohub::DeviceSynchronousProxy,
        value: String,
    ) -> Result<(), SysfsError> {
        let lock = value
            .chars()
            .next()
            .ok_or_else(|| {
                log_error!("Invalid wake lock value. String was empty.");
                sysfs_errno!(EINVAL)
            })
            .and_then(|c| match c {
                '0' => Ok(fnanohub::McuWakeLockValue::Release),
                '1' => Ok(fnanohub::McuWakeLockValue::Acquire),
                e => {
                    log_error!("Invalid wake lock value. {e:?}");
                    sysfs_error!(EINVAL)
                }
            })?;

        Ok(service.set_wake_lock(lock, zx::MonotonicInstant::INFINITE)??)
    }
}

#[derive(Default)]
pub struct WakeUpEventDuration {}

impl WakeUpEventDuration {
    fn string_to_duration(value: String) -> Result<i64, SysfsError> {
        // At the driver level, the value is expected to be within the range of u32.
        let duration_msec = value.trim().parse::<u32>().map_err(|e| {
            log_error!("Failed to parse wake up event duration: {e:?}");
            sysfs_errno!(EINVAL)
        })?;

        // The duration conversion produces a u128, but we need an i64 to provide as a zx.Duration.
        // In practice, this conversion should never fail because the driver represents this value
        // as a u32.
        let duration_ns: i64 =
            Duration::from_millis(duration_msec.into()).as_nanos().try_into().map_err(|e| {
                log_error!("Received out-of-bounds wake up event duration: {e:?}");
                sysfs_errno!(EINVAL)
            })?;

        Ok(duration_ns)
    }

    fn duration_to_string(duration: i64) -> Result<String, SysfsError> {
        // zx.Duration is an alias of i64 but we need a u64 to work the std::time::Duration.
        // In practice, this conversion should never fail because the driver represents this
        // value as a u32.
        let duration_ns: u64 = duration.try_into().map_err(|e| {
            log_error!("Received out-of-bounds wake up event duration: {e:?}");
            sysfs_errno!(EINVAL)
        })?;

        let duration_msec = Duration::from_nanos(duration_ns).as_millis();
        Ok(format!("{duration_msec}\n"))
    }
}

impl SysfsOps<fnanohub::DeviceSynchronousProxy> for WakeUpEventDuration {
    fn show(
        &self,
        service: &fidl_fuchsia_hardware_google_nanohub::DeviceSynchronousProxy,
    ) -> Result<String, SysfsError> {
        let response = service.get_wake_up_event_duration(zx::MonotonicInstant::INFINITE)??;
        WakeUpEventDuration::duration_to_string(response)
    }

    fn store(
        &self,
        service: &fidl_fuchsia_hardware_google_nanohub::DeviceSynchronousProxy,
        value: String,
    ) -> Result<(), SysfsError> {
        let duration_ns = WakeUpEventDuration::string_to_duration(value)?;
        Ok(service.set_wake_up_event_duration(duration_ns, zx::MonotonicInstant::INFINITE)??)
    }
}

trait FromBit {
    fn from_bit(bit: u8) -> Self;
}

impl FromBit for fnanohub::PinState {
    /// Construct an ISP pin state from an integer encoding.
    fn from_bit(bit: u8) -> Self {
        if bit == 0 {
            fnanohub::PinState::Low
        } else {
            fnanohub::PinState::High
        }
    }
}

trait FromBitfield {
    fn from_bitfield(bitfield: u8) -> Self;
}

impl FromBitfield for fnanohub::HardwareResetPinStates {
    /// Construct hardware reset pin states encoded in a bitfield.
    fn from_bitfield(bitfield: u8) -> Self {
        fnanohub::HardwareResetPinStates {
            isp_pin_0: fnanohub::PinState::from_bit((bitfield >> 0) & 0x1),
            isp_pin_1: fnanohub::PinState::from_bit((bitfield >> 1) & 0x1),
            isp_pin_2: fnanohub::PinState::from_bit((bitfield >> 2) & 0x1),
        }
    }
}

#[derive(Default)]
pub struct HardwareResetSysFsOps {}

impl HardwareResetSysFsOps {
    fn parse_hardware_reset_request(
        &self,
        request: &String,
    ) -> Result<fnanohub::HardwareResetPinStates, SysfsError> {
        request
            // Parse the string input into an integer...
            .trim()
            .parse::<u8>()
            .map_err(|e| {
                log_error!("Failed to parse hardware reset request: {e:?}");
                sysfs_errno!(EINVAL)
            })
            // ... then use the integer to decode the ISP pin states.
            .map(fnanohub::HardwareResetPinStates::from_bitfield)
    }
}

impl SysfsOps<fnanohub::DeviceSynchronousProxy> for HardwareResetSysFsOps {
    fn store(
        &self,
        service: &fnanohub::DeviceSynchronousProxy,
        value: String,
    ) -> Result<(), SysfsError> {
        let pin_states = self.parse_hardware_reset_request(&value)?;

        Ok(service.hardware_reset(
            pin_states.isp_pin_0,
            pin_states.isp_pin_1,
            pin_states.isp_pin_2,
            zx::MonotonicInstant::INFINITE,
        )??)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::fuchsia::test]
    fn test_wakeup_event_duration_string_to_duration_valid() {
        let msec: i64 = 123_456;
        let cases = [format!("{}", msec), format!("{}\n", msec)];

        for case in cases {
            let duration = WakeUpEventDuration::string_to_duration(case);
            assert_eq!(duration.is_ok(), true);
            assert_eq!(duration.unwrap(), msec * 1_000_000);
        }
    }

    #[::fuchsia::test]
    fn test_wakeup_event_duration_string_to_duration_invalid() {
        let ns = "foobar";
        let cases = [format!("{}", ns), format!("{}\n", ns)];

        for case in cases {
            let duration = WakeUpEventDuration::string_to_duration(case);
            assert_eq!(duration.is_err(), true);
        }
    }

    #[::fuchsia::test]
    fn test_wakeup_event_duration_duration_to_string_valid() {
        let ns: i64 = 123_456_000_000;
        let value = WakeUpEventDuration::duration_to_string(ns);
        assert_eq!(value.is_ok(), true);
        assert_eq!(value.unwrap(), format!("123456\n"));
    }

    #[::fuchsia::test]
    fn test_parse_hardware_reset_request_valid_integer() {
        let ops = HardwareResetSysFsOps::default();

        // Test all possible 3-bit values.
        for i in 0..=7 {
            let request = i.to_string();
            let pin_states = ops.parse_hardware_reset_request(&request).unwrap();

            assert_eq!(
                pin_states.isp_pin_0,
                if (i & 0x1) == 0 { fnanohub::PinState::Low } else { fnanohub::PinState::High }
            );

            assert_eq!(
                pin_states.isp_pin_1,
                if (i & 0x2) == 0 { fnanohub::PinState::Low } else { fnanohub::PinState::High }
            );

            assert_eq!(
                pin_states.isp_pin_2,
                if (i & 0x4) == 0 { fnanohub::PinState::Low } else { fnanohub::PinState::High }
            );
        }
    }

    #[::fuchsia::test]
    fn test_parse_hardware_reset_request_out_of_range_integer() {
        let ops = HardwareResetSysFsOps::default();
        let request = "8".to_string();
        let pin_states = ops.parse_hardware_reset_request(&request).unwrap();
        assert_eq!(pin_states.isp_pin_0, fnanohub::PinState::Low);
        assert_eq!(pin_states.isp_pin_1, fnanohub::PinState::Low);
        assert_eq!(pin_states.isp_pin_2, fnanohub::PinState::Low);
    }

    #[::fuchsia::test]
    fn test_parse_hardware_reset_request_invalid_string() {
        let ops = HardwareResetSysFsOps::default();
        let request = "foo".to_string();
        assert_eq!(ops.parse_hardware_reset_request(&request).is_err(), true);
    }
}
