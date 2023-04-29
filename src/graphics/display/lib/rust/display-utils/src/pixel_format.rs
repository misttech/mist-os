// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(non_upper_case_globals)]

use fidl_fuchsia_images2 as images2;
use thiserror::Error;
use {fidl_fuchsia_sysmem as sysmem, std::fmt, std::str};

// TODO(fxbug.dev/85320): This module is intended to provide some amount of compatibility between
// the sysmem pixel format (the canonical Fuchsia image format) and zx_pixel_format_t (which is
// used by the display driver stack). The `PixelFormat` type defined here suffers from the same
// component-endianness confusion as ZX_PIXEL_FORMAT_* values. Most of this module can be removed
// when the display API is changed to be in terms of sysmem image formats instead.

/// Pixel format definitions that are compatible with the display and GPU drivers' internal image
/// type representation. These are distinct from sysmem image formats and are intended to be
/// compatible with the ZX_PIXEL_FORMAT_* C definitions that are declared in
/// //zircon/system/public/zircon/pixelformat.h.
///
/// NOTE: The color-components are interpreted in memory in little-endian byte-order.
/// E.g., PixelFormat::Argb8888 as [u8; 4] would have components [blue, green, red, alpha].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PixelFormat {
    /// Default invalid image format value compatible with `ZX_PIXEL_FORMAT_NONE`.
    Unknown,
    /// 8-bit luminance-only. Identical to Gray8
    Mono8,
    /// 8-bit luminance-only. Identical to Mono8
    Gray8,
    /// 8-bit RGB 3/3/2
    Rgb332,
    /// 8-bit RGB 2/2/2
    Rgb2220,
    /// 16-bit RGB 5/6/5
    Rgb565,
    /// 24-bit RGB
    Rgb888,
    /// 32-bit BGR with ignored Alpha
    Bgr888X,
    /// 32-bit RGB with ignored Alpha
    RgbX888,
    /// 32-bit ARGB 8/8/8/8
    Argb8888,
    /// 32-bit ABGR
    Abgr8888,
    /// Bi-planar YUV (YCbCr) with 8-bit Y plane followed by an interleaved U/V plane with 2x2
    /// subsampling
    Nv12,
}

/// Alias for zx_pixel_format_t
#[allow(non_camel_case_types)]
pub type zx_pixel_format_t = u32;

/// See //zircon/system/public/zircon/pixelformat.h.
const ZX_PIXEL_FORMAT_NONE: zx_pixel_format_t = 0;
const ZX_PIXEL_FORMAT_RGB_565: zx_pixel_format_t = 0x00020001;
const ZX_PIXEL_FORMAT_RGB_332: zx_pixel_format_t = 0x00010002;
const ZX_PIXEL_FORMAT_RGB_2220: zx_pixel_format_t = 0x00010003;
const ZX_PIXEL_FORMAT_ARGB_8888: zx_pixel_format_t = 0x00040004;
const ZX_PIXEL_FORMAT_RGB_x888: zx_pixel_format_t = 0x00040005;
const ZX_PIXEL_FORMAT_MONO_8: zx_pixel_format_t = 0x00010007;
const ZX_PIXEL_FORMAT_GRAY_8: zx_pixel_format_t = 0x00010007;
const ZX_PIXEL_FORMAT_NV12: zx_pixel_format_t = 0x00010008;
const ZX_PIXEL_FORMAT_RGB_888: zx_pixel_format_t = 0x00030009;
const ZX_PIXEL_FORMAT_ABGR_8888: zx_pixel_format_t = 0x0004000a;
const ZX_PIXEL_FORMAT_BGR_888x: zx_pixel_format_t = 0x0004000b;

impl Default for PixelFormat {
    fn default() -> PixelFormat {
        PixelFormat::Unknown
    }
}

impl From<images2::PixelFormat> for PixelFormat {
    fn from(src: images2::PixelFormat) -> Self {
        match src {
            images2::PixelFormat::R8G8B8A8 => PixelFormat::Abgr8888,
            images2::PixelFormat::Bgra32 => PixelFormat::Argb8888,
            images2::PixelFormat::Nv12 => PixelFormat::Nv12,
            images2::PixelFormat::Bgr24 => PixelFormat::Rgb888,
            images2::PixelFormat::Rgb565 => PixelFormat::Rgb565,
            images2::PixelFormat::Rgb332 => PixelFormat::Rgb332,
            images2::PixelFormat::Rgb2220 => PixelFormat::Rgb2220,
            images2::PixelFormat::L8 => PixelFormat::Mono8,
            _ => PixelFormat::Unknown,
        }
    }
}

impl From<&images2::PixelFormat> for PixelFormat {
    fn from(src: &images2::PixelFormat) -> Self {
        Self::from(*src)
    }
}

impl From<PixelFormat> for zx_pixel_format_t {
    fn from(src: PixelFormat) -> Self {
        match src {
            PixelFormat::Unknown => ZX_PIXEL_FORMAT_NONE,
            PixelFormat::Mono8 => ZX_PIXEL_FORMAT_MONO_8,
            PixelFormat::Gray8 => ZX_PIXEL_FORMAT_GRAY_8,
            PixelFormat::Rgb332 => ZX_PIXEL_FORMAT_RGB_332,
            PixelFormat::Rgb2220 => ZX_PIXEL_FORMAT_RGB_2220,
            PixelFormat::Rgb565 => ZX_PIXEL_FORMAT_RGB_565,
            PixelFormat::Rgb888 => ZX_PIXEL_FORMAT_RGB_888,
            PixelFormat::Bgr888X => ZX_PIXEL_FORMAT_BGR_888x,
            PixelFormat::RgbX888 => ZX_PIXEL_FORMAT_RGB_x888,
            PixelFormat::Abgr8888 => ZX_PIXEL_FORMAT_ABGR_8888,
            PixelFormat::Argb8888 => ZX_PIXEL_FORMAT_ARGB_8888,
            PixelFormat::Nv12 => ZX_PIXEL_FORMAT_NV12,
        }
    }
}

impl From<&PixelFormat> for zx_pixel_format_t {
    fn from(src: &PixelFormat) -> Self {
        Self::from(*src)
    }
}

impl From<PixelFormat> for sysmem::PixelFormatType {
    fn from(src: PixelFormat) -> Self {
        match src {
            PixelFormat::Unknown => Self::Invalid,
            PixelFormat::Mono8 | PixelFormat::Gray8 => Self::L8,
            PixelFormat::Rgb332 => Self::Rgb332,
            PixelFormat::Rgb2220 => Self::Rgb2220,
            PixelFormat::Rgb565 => Self::Rgb565,
            PixelFormat::Rgb888 => Self::Bgr24,
            PixelFormat::Abgr8888 | PixelFormat::Bgr888X => Self::R8G8B8A8,
            PixelFormat::Argb8888 | PixelFormat::RgbX888 => Self::Bgra32,
            PixelFormat::Nv12 => Self::Nv12,
        }
    }
}

impl From<PixelFormat> for images2::PixelFormat {
    fn from(src: PixelFormat) -> Self {
        match src {
            PixelFormat::Unknown => Self::Invalid,
            PixelFormat::Mono8 | PixelFormat::Gray8 => Self::L8,
            PixelFormat::Rgb332 => Self::Rgb332,
            PixelFormat::Rgb2220 => Self::Rgb2220,
            PixelFormat::Rgb565 => Self::Rgb565,
            PixelFormat::Rgb888 => Self::Bgr24,
            PixelFormat::Abgr8888 | PixelFormat::Bgr888X => Self::R8G8B8A8,
            PixelFormat::Argb8888 | PixelFormat::RgbX888 => Self::Bgra32,
            PixelFormat::Nv12 => Self::Nv12,
        }
    }
}

impl From<&PixelFormat> for images2::PixelFormat {
    fn from(src: &PixelFormat) -> Self {
        Self::from(*src)
    }
}

impl fmt::Display for PixelFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let string: &str = match *self {
            PixelFormat::Unknown => "(unknown)",
            PixelFormat::Mono8 => "Mono 8-bit",
            PixelFormat::Gray8 => "Gray 8-bit",
            PixelFormat::Rgb332 => "RGB 332",
            PixelFormat::Rgb2220 => "RGB 2220",
            PixelFormat::Rgb565 => "RGB 565",
            PixelFormat::Rgb888 => "RGB 888",
            PixelFormat::Bgr888X => "BGR 888x",
            PixelFormat::RgbX888 => "RGB x888",
            PixelFormat::Abgr8888 => "ABGR 8888",
            PixelFormat::Argb8888 => "ARGB 8888",
            PixelFormat::Nv12 => "NV12",
        };
        write!(f, "{}", string)
    }
}

#[derive(Debug, PartialEq)]
pub enum ParsePixelFormatError {
    UnsupportedFormat(String),
}

impl fmt::Display for ParsePixelFormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParsePixelFormatError::UnsupportedFormat(s) => write!(f, "Unsupported format {}", s),
        }
    }
}

impl str::FromStr for PixelFormat {
    type Err = ParsePixelFormatError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "unknown" => Ok(PixelFormat::Unknown),
            "mono8" => Ok(PixelFormat::Mono8),
            "gray8" => Ok(PixelFormat::Gray8),
            "rgb332" => Ok(PixelFormat::Rgb332),
            "rgb2220" => Ok(PixelFormat::Rgb2220),
            "rgb565" => Ok(PixelFormat::Rgb565),
            "rgb888" => Ok(PixelFormat::Rgb888),
            "bgr888x" => Ok(PixelFormat::Bgr888X),
            "rgbx888" => Ok(PixelFormat::RgbX888),
            "abgr8888" => Ok(PixelFormat::Abgr8888),
            "argb8888" => Ok(PixelFormat::Argb8888),
            "nv12" => Ok(PixelFormat::Nv12),
            _ => Err(ParsePixelFormatError::UnsupportedFormat(s.to_string())),
        }
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum GetBytesPerPixelError {
    #[error("Unsupported format: {0}")]
    UnsupportedFormat(PixelFormat),
}

/// Get number of bytes per pixel for given `pixel_format`.
/// Currently only packed pixel formats are supported. Planar formats (e.g.
/// NV12) are not supported.
pub fn get_bytes_per_pixel(pixel_format: PixelFormat) -> Result<usize, GetBytesPerPixelError> {
    match pixel_format {
        PixelFormat::Mono8 | PixelFormat::Gray8 | PixelFormat::Rgb332 | PixelFormat::Rgb2220 => {
            Ok(1)
        }
        PixelFormat::Rgb565 => Ok(2),
        PixelFormat::Rgb888 => Ok(3),
        PixelFormat::Bgr888X
        | PixelFormat::RgbX888
        | PixelFormat::Argb8888
        | PixelFormat::Abgr8888 => Ok(4),
        _ => Err(GetBytesPerPixelError::UnsupportedFormat(pixel_format)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[fuchsia::test]
    fn pixel_format_from_str_invalid() {
        assert_eq!(
            PixelFormat::from_str("bad"),
            Err(ParsePixelFormatError::UnsupportedFormat("bad".to_string()))
        );
        assert_eq!(
            PixelFormat::from_str("rgba8888"),
            Err(ParsePixelFormatError::UnsupportedFormat("rgba8888".to_string()))
        );
    }

    #[fuchsia::test]
    fn pixel_format_from_str_valid() {
        assert_eq!(PixelFormat::from_str("unknown"), Ok(PixelFormat::Unknown));
        assert_eq!(PixelFormat::from_str("mono8"), Ok(PixelFormat::Mono8));
        assert_eq!(PixelFormat::from_str("gray8"), Ok(PixelFormat::Gray8));
        assert_eq!(PixelFormat::from_str("rgb332"), Ok(PixelFormat::Rgb332));
        assert_eq!(PixelFormat::from_str("rgb2220"), Ok(PixelFormat::Rgb2220));
        assert_eq!(PixelFormat::from_str("rgb565"), Ok(PixelFormat::Rgb565));
        assert_eq!(PixelFormat::from_str("rgb888"), Ok(PixelFormat::Rgb888));
        assert_eq!(PixelFormat::from_str("bgr888x"), Ok(PixelFormat::Bgr888X));
        assert_eq!(PixelFormat::from_str("rgbx888"), Ok(PixelFormat::RgbX888));
        assert_eq!(PixelFormat::from_str("abgr8888"), Ok(PixelFormat::Abgr8888));
        assert_eq!(PixelFormat::from_str("argb8888"), Ok(PixelFormat::Argb8888));
        assert_eq!(PixelFormat::from_str("nv12"), Ok(PixelFormat::Nv12));

        assert_eq!(PixelFormat::from_str("Unknown"), Ok(PixelFormat::Unknown));
        assert_eq!(PixelFormat::from_str("Mono8"), Ok(PixelFormat::Mono8));
        assert_eq!(PixelFormat::from_str("Gray8"), Ok(PixelFormat::Gray8));
        assert_eq!(PixelFormat::from_str("Rgb332"), Ok(PixelFormat::Rgb332));
        assert_eq!(PixelFormat::from_str("Rgb2220"), Ok(PixelFormat::Rgb2220));
        assert_eq!(PixelFormat::from_str("Rgb565"), Ok(PixelFormat::Rgb565));
        assert_eq!(PixelFormat::from_str("Rgb888"), Ok(PixelFormat::Rgb888));
        assert_eq!(PixelFormat::from_str("Bgr888x"), Ok(PixelFormat::Bgr888X));
        assert_eq!(PixelFormat::from_str("Rgbx888"), Ok(PixelFormat::RgbX888));
        assert_eq!(PixelFormat::from_str("Abgr8888"), Ok(PixelFormat::Abgr8888));
        assert_eq!(PixelFormat::from_str("Argb8888"), Ok(PixelFormat::Argb8888));
        assert_eq!(PixelFormat::from_str("Nv12"), Ok(PixelFormat::Nv12));

        assert_eq!(PixelFormat::from_str("UNKNOWN"), Ok(PixelFormat::Unknown));
        assert_eq!(PixelFormat::from_str("MONO8"), Ok(PixelFormat::Mono8));
        assert_eq!(PixelFormat::from_str("GRAY8"), Ok(PixelFormat::Gray8));
        assert_eq!(PixelFormat::from_str("RGB332"), Ok(PixelFormat::Rgb332));
        assert_eq!(PixelFormat::from_str("RGB2220"), Ok(PixelFormat::Rgb2220));
        assert_eq!(PixelFormat::from_str("RGB565"), Ok(PixelFormat::Rgb565));
        assert_eq!(PixelFormat::from_str("RGB888"), Ok(PixelFormat::Rgb888));
        assert_eq!(PixelFormat::from_str("BGR888X"), Ok(PixelFormat::Bgr888X));
        assert_eq!(PixelFormat::from_str("RGBX888"), Ok(PixelFormat::RgbX888));
        assert_eq!(PixelFormat::from_str("ABGR8888"), Ok(PixelFormat::Abgr8888));
        assert_eq!(PixelFormat::from_str("ARGB8888"), Ok(PixelFormat::Argb8888));
        assert_eq!(PixelFormat::from_str("NV12"), Ok(PixelFormat::Nv12));
    }

    #[fuchsia::test]
    fn get_bytes_per_pixel_valid() {
        assert_eq!(get_bytes_per_pixel(PixelFormat::Mono8), Ok(1));
        assert_eq!(get_bytes_per_pixel(PixelFormat::Gray8), Ok(1));
        assert_eq!(get_bytes_per_pixel(PixelFormat::Rgb332), Ok(1));
        assert_eq!(get_bytes_per_pixel(PixelFormat::Rgb2220), Ok(1));
        assert_eq!(get_bytes_per_pixel(PixelFormat::Rgb565), Ok(2));
        assert_eq!(get_bytes_per_pixel(PixelFormat::Rgb888), Ok(3));
        assert_eq!(get_bytes_per_pixel(PixelFormat::Bgr888X), Ok(4));
        assert_eq!(get_bytes_per_pixel(PixelFormat::RgbX888), Ok(4));
        assert_eq!(get_bytes_per_pixel(PixelFormat::Argb8888), Ok(4));
        assert_eq!(get_bytes_per_pixel(PixelFormat::Abgr8888), Ok(4));
    }

    #[fuchsia::test]
    fn get_bytes_per_pixel_invalid() {
        assert_eq!(
            get_bytes_per_pixel(PixelFormat::Nv12),
            Err(GetBytesPerPixelError::UnsupportedFormat(PixelFormat::Nv12))
        );
        assert_eq!(
            get_bytes_per_pixel(PixelFormat::Unknown),
            Err(GetBytesPerPixelError::UnsupportedFormat(PixelFormat::Unknown))
        );
    }

    #[fuchsia::test]
    fn sysmem2_pixel_format_conversion() {
        assert_eq!(
            images2::PixelFormat::from(PixelFormat::from(images2::PixelFormat::R8G8B8A8)),
            images2::PixelFormat::R8G8B8A8
        );
        assert_eq!(
            images2::PixelFormat::from(PixelFormat::from(images2::PixelFormat::Bgra32)),
            images2::PixelFormat::Bgra32
        );
        assert_eq!(
            images2::PixelFormat::from(PixelFormat::from(images2::PixelFormat::Nv12)),
            images2::PixelFormat::Nv12
        );
        assert_eq!(
            images2::PixelFormat::from(PixelFormat::from(images2::PixelFormat::Bgr24)),
            images2::PixelFormat::Bgr24
        );
        assert_eq!(
            images2::PixelFormat::from(PixelFormat::from(images2::PixelFormat::Rgb565)),
            images2::PixelFormat::Rgb565
        );
        assert_eq!(
            images2::PixelFormat::from(PixelFormat::from(images2::PixelFormat::Rgb332)),
            images2::PixelFormat::Rgb332
        );
        assert_eq!(
            images2::PixelFormat::from(PixelFormat::from(images2::PixelFormat::Rgb2220)),
            images2::PixelFormat::Rgb2220
        );
        assert_eq!(
            images2::PixelFormat::from(PixelFormat::from(images2::PixelFormat::L8)),
            images2::PixelFormat::L8
        );
    }
}
