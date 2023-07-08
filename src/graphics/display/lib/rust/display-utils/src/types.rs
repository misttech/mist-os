// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{error::Result, pixel_format::PixelFormat};
use {
    fidl_fuchsia_hardware_display::{
        BufferCollectionId as FidlCollectionId, DisplayId as FidlDisplayId, EventId as FidlEventId,
        ImageId as FidlImageId, Info, LayerId as FidlLayerId, INVALID_DISP_ID,
    },
    fuchsia_async::OnSignals,
    fuchsia_zircon::{self as zx, AsHandleRef},
    std::fmt,
};

/// Strongly typed wrapper around a display ID.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct DisplayId(pub u64);

/// Represents an invalid DisplayId value.
pub const INVALID_DISPLAY_ID: DisplayId = DisplayId(INVALID_DISP_ID);

impl Default for DisplayId {
    fn default() -> Self {
        INVALID_DISPLAY_ID
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventId(pub u64);

/// Represents an invalid EventId value.
pub const INVALID_EVENT_ID: EventId = EventId(INVALID_DISP_ID);

impl Default for EventId {
    fn default() -> Self {
        INVALID_EVENT_ID
    }
}

impl From<FidlEventId> for EventId {
    fn from(fidl_event_id: FidlEventId) -> Self {
        EventId(fidl_event_id.value)
    }
}

impl From<EventId> for FidlEventId {
    fn from(event_id: EventId) -> Self {
        FidlEventId { value: event_id.0 }
    }
}

/// Strongly typed wrapper around a display layer ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LayerId(pub u64);

/// Represents an invalid LayerId value.
pub const INVALID_LAYER_ID: LayerId = LayerId(INVALID_DISP_ID);

impl Default for LayerId {
    fn default() -> Self {
        INVALID_LAYER_ID
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
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ImageId(pub u64);

/// Represents an invalid ImageId value.
pub const INVALID_IMAGE_ID: ImageId = ImageId(INVALID_DISP_ID);

impl Default for ImageId {
    fn default() -> Self {
        INVALID_IMAGE_ID
    }
}

impl From<FidlImageId> for ImageId {
    fn from(fidl_image_id: FidlImageId) -> Self {
        ImageId(fidl_image_id.value)
    }
}

impl From<ImageId> for FidlImageId {
    fn from(image_id: ImageId) -> Self {
        FidlImageId { value: image_id.0 }
    }
}

/// Strongly typed wrapper around a sysmem buffer collection ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CollectionId(pub u64);

impl From<FidlCollectionId> for CollectionId {
    fn from(fidl_collection_id: FidlCollectionId) -> Self {
        CollectionId(fidl_collection_id.value)
    }
}

impl From<CollectionId> for FidlCollectionId {
    fn from(collection_id: CollectionId) -> Self {
        FidlCollectionId { value: collection_id.0 }
    }
}

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
        assert_eq!(INVALID_LAYER_ID, LayerId::from(FidlLayerId { value: INVALID_DISP_ID }));
    }

    #[fuchsia::test]
    fn fidl_layer_id_from_layer_id() {
        assert_eq!(FidlLayerId { value: 1 }, FidlLayerId::from(LayerId(1)));
        assert_eq!(FidlLayerId { value: 2 }, FidlLayerId::from(LayerId(2)));
        const LARGE: u64 = 1 << 63;
        assert_eq!(FidlLayerId { value: LARGE }, FidlLayerId::from(LayerId(LARGE)));
        assert_eq!(FidlLayerId { value: INVALID_DISP_ID }, FidlLayerId::from(INVALID_LAYER_ID));
    }

    #[fuchsia::test]
    fn fidl_layer_id_to_layer_id() {
        assert_eq!(LayerId(1), FidlLayerId { value: 1 }.into());
        assert_eq!(LayerId(2), FidlLayerId { value: 2 }.into());
        const LARGE: u64 = 1 << 63;
        assert_eq!(LayerId(LARGE), FidlLayerId { value: LARGE }.into());
        assert_eq!(INVALID_LAYER_ID, FidlLayerId { value: INVALID_DISP_ID }.into());
    }

    #[fuchsia::test]
    fn layer_id_to_fidl_layer_id() {
        assert_eq!(FidlLayerId { value: 1 }, LayerId(1).into());
        assert_eq!(FidlLayerId { value: 2 }, LayerId(2).into());
        const LARGE: u64 = 1 << 63;
        assert_eq!(FidlLayerId { value: LARGE }, LayerId(LARGE).into());
        assert_eq!(FidlLayerId { value: INVALID_DISP_ID }, INVALID_LAYER_ID.into());
    }

    #[fuchsia::test]
    fn layer_id_default() {
        let default: LayerId = Default::default();
        assert_eq!(default, INVALID_LAYER_ID);
    }

    #[fuchsia::test]
    fn display_id_from_fidl_display_id() {
        assert_eq!(DisplayId(1), DisplayId::from(FidlDisplayId { value: 1 }));
        assert_eq!(DisplayId(2), DisplayId::from(FidlDisplayId { value: 2 }));
        const LARGE: u64 = 1 << 63;
        assert_eq!(DisplayId(LARGE), DisplayId::from(FidlDisplayId { value: LARGE }));
        assert_eq!(INVALID_DISPLAY_ID, DisplayId::from(FidlDisplayId { value: INVALID_DISP_ID }));
    }

    #[fuchsia::test]
    fn fidl_display_id_from_display_id() {
        assert_eq!(FidlDisplayId { value: 1 }, FidlDisplayId::from(DisplayId(1)));
        assert_eq!(FidlDisplayId { value: 2 }, FidlDisplayId::from(DisplayId(2)));
        const LARGE: u64 = 1 << 63;
        assert_eq!(FidlDisplayId { value: LARGE }, FidlDisplayId::from(DisplayId(LARGE)));
        assert_eq!(
            FidlDisplayId { value: INVALID_DISP_ID },
            FidlDisplayId::from(INVALID_DISPLAY_ID)
        );
    }

    #[fuchsia::test]
    fn fidl_display_id_to_display_id() {
        assert_eq!(DisplayId(1), FidlDisplayId { value: 1 }.into());
        assert_eq!(DisplayId(2), FidlDisplayId { value: 2 }.into());
        const LARGE: u64 = 1 << 63;
        assert_eq!(DisplayId(LARGE), FidlDisplayId { value: LARGE }.into());
        assert_eq!(INVALID_DISPLAY_ID, FidlDisplayId { value: INVALID_DISP_ID }.into());
    }

    #[fuchsia::test]
    fn display_id_to_fidl_display_id() {
        assert_eq!(FidlDisplayId { value: 1 }, DisplayId(1).into());
        assert_eq!(FidlDisplayId { value: 2 }, DisplayId(2).into());
        const LARGE: u64 = 1 << 63;
        assert_eq!(FidlDisplayId { value: LARGE }, DisplayId(LARGE).into());
        assert_eq!(FidlDisplayId { value: INVALID_DISP_ID }, INVALID_DISPLAY_ID.into());
    }

    #[fuchsia::test]
    fn display_id_default() {
        let default: DisplayId = Default::default();
        assert_eq!(default, INVALID_DISPLAY_ID);
    }

    #[fuchsia::test]
    fn collection_id_from_fidl_collection_id() {
        assert_eq!(CollectionId(1), CollectionId::from(FidlCollectionId { value: 1 }));
        assert_eq!(CollectionId(2), CollectionId::from(FidlCollectionId { value: 2 }));
        const LARGE: u64 = 1 << 63;
        assert_eq!(CollectionId(LARGE), CollectionId::from(FidlCollectionId { value: LARGE }));
    }

    #[fuchsia::test]
    fn fidl_collection_id_from_collection_id() {
        assert_eq!(FidlCollectionId { value: 1 }, FidlCollectionId::from(CollectionId(1)));
        assert_eq!(FidlCollectionId { value: 2 }, FidlCollectionId::from(CollectionId(2)));
        const LARGE: u64 = 1 << 63;
        assert_eq!(FidlCollectionId { value: LARGE }, FidlCollectionId::from(CollectionId(LARGE)));
    }

    #[fuchsia::test]
    fn fidl_collection_id_to_collection_id() {
        assert_eq!(CollectionId(1), FidlCollectionId { value: 1 }.into());
        assert_eq!(CollectionId(2), FidlCollectionId { value: 2 }.into());
        const LARGE: u64 = 1 << 63;
        assert_eq!(CollectionId(LARGE), FidlCollectionId { value: LARGE }.into());
    }

    #[fuchsia::test]
    fn collection_id_to_fidl_collection_id() {
        assert_eq!(FidlCollectionId { value: 1 }, CollectionId(1).into());
        assert_eq!(FidlCollectionId { value: 2 }, CollectionId(2).into());
        const LARGE: u64 = 1 << 63;
        assert_eq!(FidlCollectionId { value: LARGE }, CollectionId(LARGE).into());
    }

    #[fuchsia::test]
    fn event_id_from_fidl_event_id() {
        assert_eq!(EventId(1), EventId::from(FidlEventId { value: 1 }));
        assert_eq!(EventId(2), EventId::from(FidlEventId { value: 2 }));
        const LARGE: u64 = 1 << 63;
        assert_eq!(EventId(LARGE), EventId::from(FidlEventId { value: LARGE }));
        assert_eq!(INVALID_EVENT_ID, EventId::from(FidlEventId { value: INVALID_DISP_ID }));
    }

    #[fuchsia::test]
    fn fidl_event_id_from_event_id() {
        assert_eq!(FidlEventId { value: 1 }, FidlEventId::from(EventId(1)));
        assert_eq!(FidlEventId { value: 2 }, FidlEventId::from(EventId(2)));
        const LARGE: u64 = 1 << 63;
        assert_eq!(FidlEventId { value: LARGE }, FidlEventId::from(EventId(LARGE)));
        assert_eq!(FidlEventId { value: INVALID_DISP_ID }, FidlEventId::from(INVALID_EVENT_ID));
    }

    #[fuchsia::test]
    fn fidl_event_id_to_event_id() {
        assert_eq!(EventId(1), FidlEventId { value: 1 }.into());
        assert_eq!(EventId(2), FidlEventId { value: 2 }.into());
        const LARGE: u64 = 1 << 63;
        assert_eq!(EventId(LARGE), FidlEventId { value: LARGE }.into());
        assert_eq!(INVALID_EVENT_ID, FidlEventId { value: INVALID_DISP_ID }.into());
    }

    #[fuchsia::test]
    fn event_id_to_fidl_event_id() {
        assert_eq!(FidlEventId { value: 1 }, EventId(1).into());
        assert_eq!(FidlEventId { value: 2 }, EventId(2).into());
        const LARGE: u64 = 1 << 63;
        assert_eq!(FidlEventId { value: LARGE }, EventId(LARGE).into());
        assert_eq!(FidlEventId { value: INVALID_DISP_ID }, INVALID_EVENT_ID.into());
    }

    #[fuchsia::test]
    fn event_id_default() {
        let default: EventId = Default::default();
        assert_eq!(default, INVALID_EVENT_ID);
    }

    #[fuchsia::test]
    fn image_id_from_fidl_image_id() {
        assert_eq!(ImageId(1), ImageId::from(FidlImageId { value: 1 }));
        assert_eq!(ImageId(2), ImageId::from(FidlImageId { value: 2 }));
        const LARGE: u64 = 1 << 63;
        assert_eq!(ImageId(LARGE), ImageId::from(FidlImageId { value: LARGE }));
        assert_eq!(INVALID_IMAGE_ID, ImageId::from(FidlImageId { value: INVALID_DISP_ID }));
    }

    #[fuchsia::test]
    fn fidl_image_id_from_image_id() {
        assert_eq!(FidlImageId { value: 1 }, FidlImageId::from(ImageId(1)));
        assert_eq!(FidlImageId { value: 2 }, FidlImageId::from(ImageId(2)));
        const LARGE: u64 = 1 << 63;
        assert_eq!(FidlImageId { value: LARGE }, FidlImageId::from(ImageId(LARGE)));
        assert_eq!(FidlImageId { value: INVALID_DISP_ID }, FidlImageId::from(INVALID_IMAGE_ID));
    }

    #[fuchsia::test]
    fn fidl_image_id_to_image_id() {
        assert_eq!(ImageId(1), FidlImageId { value: 1 }.into());
        assert_eq!(ImageId(2), FidlImageId { value: 2 }.into());
        const LARGE: u64 = 1 << 63;
        assert_eq!(ImageId(LARGE), FidlImageId { value: LARGE }.into());
        assert_eq!(INVALID_IMAGE_ID, FidlImageId { value: INVALID_DISP_ID }.into());
    }

    #[fuchsia::test]
    fn image_id_to_fidl_image_id() {
        assert_eq!(FidlImageId { value: 1 }, ImageId(1).into());
        assert_eq!(FidlImageId { value: 2 }, ImageId(2).into());
        const LARGE: u64 = 1 << 63;
        assert_eq!(FidlImageId { value: LARGE }, ImageId(LARGE).into());
        assert_eq!(FidlImageId { value: INVALID_DISP_ID }, INVALID_IMAGE_ID.into());
    }

    #[fuchsia::test]
    fn image_id_default() {
        let default: ImageId = Default::default();
        assert_eq!(default, INVALID_IMAGE_ID);
    }
}
