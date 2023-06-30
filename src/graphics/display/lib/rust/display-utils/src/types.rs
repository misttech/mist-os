// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{error::Result, pixel_format::PixelFormat};
use {
    fidl_fuchsia_hardware_display::{
        DisplayId as FidlDisplayId, Info, LayerId as FidlLayerId, INVALID_DISP_ID,
    },
    fuchsia_async::OnSignals,
    fuchsia_zircon::{self as zx, AsHandleRef},
    std::fmt,
};

/// Strongly typed wrapper around a display ID.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct DisplayId(pub u64);

impl Default for DisplayId {
    fn default() -> Self {
        DisplayId(INVALID_DISP_ID)
    }
}

impl From<FidlDisplayId> for DisplayId {
    fn from(fidl_display_id: FidlDisplayId) -> Self {
        DisplayId(fidl_display_id.value)
    }
}

impl From<DisplayId> for FidlDisplayId {
    fn from(display_id: DisplayId) -> Self {
        FidlDisplayId { value: display_id.0 }
    }
}

/// Strongly typed wrapper around a display driver event ID.
#[derive(Clone, Copy, Debug)]
pub struct EventId(pub u64);

/// Strongly typed wrapper around a display layer ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayerId(pub u64);

impl Default for LayerId {
    fn default() -> Self {
        LayerId(INVALID_DISP_ID)
    }
}

impl From<FidlLayerId> for LayerId {
    fn from(fidl_layer_id: FidlLayerId) -> Self {
        LayerId(fidl_layer_id.value)
    }
}

impl From<LayerId> for FidlLayerId {
    fn from(layer_id: LayerId) -> Self {
        FidlLayerId { value: layer_id.0 }
    }
}

/// Strongly typed wrapper around an image ID.
#[derive(Clone, Copy, Debug)]
pub struct ImageId(pub u64);

/// Strongly typed wrapper around a sysmem buffer collection ID.
#[derive(Clone, Copy, Debug)]
pub struct CollectionId(pub u64);

/// Enhances the `fuchsia.hardware.display.Info` FIDL struct.
#[derive(Clone, Debug)]
pub struct DisplayInfo(pub Info);

impl DisplayInfo {
    /// Returns the ID for this display.
    pub fn id(&self) -> DisplayId {
        self.0.id.into()
    }
}

/// Custom user-friendly format representation.
impl fmt::Display for DisplayInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Display (id: {})", self.0.id.value)?;
        writeln!(f, "\tManufacturer Name: \"{}\"", self.0.manufacturer_name)?;
        writeln!(f, "\tMonitor Name: \"{}\"", self.0.monitor_name)?;
        writeln!(f, "\tMonitor Serial: \"{}\"", self.0.monitor_serial)?;
        writeln!(
            f,
            "\tPhysical Dimensions: {}mm x {}mm",
            self.0.horizontal_size_mm, self.0.vertical_size_mm
        )?;

        writeln!(f, "\tPixel Formats:")?;
        for (i, format) in self.0.pixel_format.iter().map(PixelFormat::from).enumerate() {
            writeln!(f, "\t\t{}:\t{}", i, format)?;
        }

        writeln!(f, "\tDisplay Modes:")?;
        for (i, mode) in self.0.modes.iter().enumerate() {
            writeln!(
                f,
                "\t\t{}:\t{:.2} Hz @ {}x{}",
                i,
                (mode.refresh_rate_e2 as f32) / 100.,
                mode.horizontal_resolution,
                mode.vertical_resolution
            )?;
        }
        writeln!(f, "\tCursor Configurations:")?;
        for (i, config) in self.0.cursor_configs.iter().enumerate() {
            writeln!(
                f,
                "\t\t{}:\t{} - {}x{}",
                i,
                PixelFormat::from(config.pixel_format),
                config.width,
                config.height
            )?;
        }

        write!(f, "")
    }
}

/// A zircon event that has been registered with the display driver.
pub struct Event {
    id: EventId,
    event: zx::Event,
}

impl Event {
    pub(crate) fn new(id: EventId, event: zx::Event) -> Event {
        Event { id, event }
    }

    /// Returns the ID for this event.
    pub fn id(&self) -> EventId {
        self.id
    }

    /// Returns a future that completes when the event has been signaled.
    pub async fn wait(&self) -> Result<()> {
        OnSignals::new(&self.event, zx::Signals::EVENT_SIGNALED).await?;
        self.event.as_handle_ref().signal(zx::Signals::EVENT_SIGNALED, zx::Signals::NONE)?;
        Ok(())
    }

    /// Signals the event.
    pub fn signal(&self) -> Result<()> {
        self.event.as_handle_ref().signal(zx::Signals::NONE, zx::Signals::EVENT_SIGNALED)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn layer_id_from_fidl_layer_id() {
        assert_eq!(LayerId(1), LayerId::from(FidlLayerId { value: 1 }));
        assert_eq!(LayerId(2), LayerId::from(FidlLayerId { value: 2 }));
        const LARGE: u64 = 1 << 63;
        assert_eq!(LayerId(LARGE), LayerId::from(FidlLayerId { value: LARGE }));
    }

    #[fuchsia::test]
    fn fidl_layer_id_from_layer_id() {
        assert_eq!(FidlLayerId { value: 1 }, FidlLayerId::from(LayerId(1)));
        assert_eq!(FidlLayerId { value: 2 }, FidlLayerId::from(LayerId(2)));
        const LARGE: u64 = 1 << 63;
        assert_eq!(FidlLayerId { value: LARGE }, FidlLayerId::from(LayerId(LARGE)));
    }

    #[fuchsia::test]
    fn fidl_layer_id_to_layer_id() {
        assert_eq!(LayerId(1), FidlLayerId { value: 1 }.into());
        assert_eq!(LayerId(2), FidlLayerId { value: 2 }.into());
        const LARGE: u64 = 1 << 63;
        assert_eq!(LayerId(LARGE), FidlLayerId { value: LARGE }.into());
    }

    #[fuchsia::test]
    fn layer_id_to_fidl_layer_id() {
        assert_eq!(FidlLayerId { value: 1 }, LayerId(1).into());
        assert_eq!(FidlLayerId { value: 2 }, LayerId(2).into());
        const LARGE: u64 = 1 << 63;
        assert_eq!(FidlLayerId { value: LARGE }, LayerId(LARGE).into());
    }

    #[fuchsia::test]
    fn layer_id_default() {
        let default: LayerId = Default::default();
        assert_eq!(default, LayerId(INVALID_DISP_ID));
    }

    #[fuchsia::test]
    fn display_id_from_fidl_display_id() {
        assert_eq!(DisplayId(1), DisplayId::from(FidlDisplayId { value: 1 }));
        assert_eq!(DisplayId(2), DisplayId::from(FidlDisplayId { value: 2 }));
        const LARGE: u64 = 1 << 63;
        assert_eq!(DisplayId(LARGE), DisplayId::from(FidlDisplayId { value: LARGE }));
    }

    #[fuchsia::test]
    fn fidl_display_id_from_display_id() {
        assert_eq!(FidlDisplayId { value: 1 }, FidlDisplayId::from(DisplayId(1)));
        assert_eq!(FidlDisplayId { value: 2 }, FidlDisplayId::from(DisplayId(2)));
        const LARGE: u64 = 1 << 63;
        assert_eq!(FidlDisplayId { value: LARGE }, FidlDisplayId::from(DisplayId(LARGE)));
    }

    #[fuchsia::test]
    fn fidl_display_id_to_display_id() {
        assert_eq!(DisplayId(1), FidlDisplayId { value: 1 }.into());
        assert_eq!(DisplayId(2), FidlDisplayId { value: 2 }.into());
        const LARGE: u64 = 1 << 63;
        assert_eq!(DisplayId(LARGE), FidlDisplayId { value: LARGE }.into());
    }

    #[fuchsia::test]
    fn display_id_to_fidl_display_id() {
        assert_eq!(FidlDisplayId { value: 1 }, DisplayId(1).into());
        assert_eq!(FidlDisplayId { value: 2 }, DisplayId(2).into());
        const LARGE: u64 = 1 << 63;
        assert_eq!(FidlDisplayId { value: LARGE }, DisplayId(LARGE).into());
    }

    #[fuchsia::test]
    fn display_id_default() {
        let default: DisplayId = Default::default();
        assert_eq!(default, DisplayId(INVALID_DISP_ID));
    }
}
