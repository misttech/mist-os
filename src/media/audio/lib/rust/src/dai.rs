// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_hardware_audio as fhaudio;
use std::fmt::Display;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct DaiFormatSet {
    pub number_of_channels: Vec<u32>,
    pub sample_formats: Vec<DaiSampleFormat>,
    pub frame_formats: Vec<DaiFrameFormat>,
    pub frame_rates: Vec<u32>,
    pub bits_per_slot: Vec<u8>,
    pub bits_per_sample: Vec<u8>,
}

impl From<fhaudio::DaiSupportedFormats> for DaiFormatSet {
    fn from(value: fhaudio::DaiSupportedFormats) -> Self {
        Self {
            number_of_channels: value.number_of_channels,
            sample_formats: value.sample_formats.into_iter().map(From::from).collect(),
            frame_formats: value.frame_formats.into_iter().map(From::from).collect(),
            frame_rates: value.frame_rates,
            bits_per_slot: value.bits_per_slot,
            bits_per_sample: value.bits_per_sample,
        }
    }
}

impl From<DaiFormatSet> for fhaudio::DaiSupportedFormats {
    fn from(value: DaiFormatSet) -> Self {
        Self {
            number_of_channels: value.number_of_channels,
            sample_formats: value.sample_formats.into_iter().map(From::from).collect(),
            frame_formats: value.frame_formats.into_iter().map(From::from).collect(),
            frame_rates: value.frame_rates,
            bits_per_slot: value.bits_per_slot,
            bits_per_sample: value.bits_per_sample,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaiSampleFormat {
    Pdm,
    PcmSigned,
    PcmUnsigned,
    PcmFloat,
}

impl Display for DaiSampleFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DaiSampleFormat::Pdm => "pdm",
            DaiSampleFormat::PcmSigned => "pcm_signed",
            DaiSampleFormat::PcmUnsigned => "pcm_unsigned",
            DaiSampleFormat::PcmFloat => "pcm_float",
        };
        f.write_str(s)
    }
}

impl From<fhaudio::DaiSampleFormat> for DaiSampleFormat {
    fn from(value: fhaudio::DaiSampleFormat) -> Self {
        match value {
            fhaudio::DaiSampleFormat::Pdm => Self::Pdm,
            fhaudio::DaiSampleFormat::PcmSigned => Self::PcmSigned,
            fhaudio::DaiSampleFormat::PcmUnsigned => Self::PcmUnsigned,
            fhaudio::DaiSampleFormat::PcmFloat => Self::PcmFloat,
        }
    }
}

impl From<DaiSampleFormat> for fhaudio::DaiSampleFormat {
    fn from(value: DaiSampleFormat) -> Self {
        match value {
            DaiSampleFormat::Pdm => Self::Pdm,
            DaiSampleFormat::PcmSigned => Self::PcmSigned,
            DaiSampleFormat::PcmUnsigned => Self::PcmUnsigned,
            DaiSampleFormat::PcmFloat => Self::PcmFloat,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaiFrameFormat {
    Standard(DaiFrameFormatStandard),
    Custom(DaiFrameFormatCustom),
}

impl Display for DaiFrameFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaiFrameFormat::Standard(standard) => standard.fmt(f),
            DaiFrameFormat::Custom(custom) => {
                f.write_str("custom:")?;
                custom.fmt(f)
            }
        }
    }
}

impl From<fhaudio::DaiFrameFormat> for DaiFrameFormat {
    fn from(value: fhaudio::DaiFrameFormat) -> Self {
        match value {
            fhaudio::DaiFrameFormat::FrameFormatStandard(standard) => {
                Self::Standard(standard.into())
            }
            fhaudio::DaiFrameFormat::FrameFormatCustom(custom) => Self::Custom(custom.into()),
        }
    }
}

impl From<DaiFrameFormat> for fhaudio::DaiFrameFormat {
    fn from(value: DaiFrameFormat) -> Self {
        match value {
            DaiFrameFormat::Standard(standard) => Self::FrameFormatStandard(standard.into()),
            DaiFrameFormat::Custom(custom) => Self::FrameFormatCustom(custom.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaiFrameFormatStandard {
    None,
    I2S,
    StereoLeft,
    StereoRight,
    Tdm1,
    Tdm2,
    Tdm3,
}

impl Display for DaiFrameFormatStandard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DaiFrameFormatStandard::None => "none",
            DaiFrameFormatStandard::I2S => "i2s",
            DaiFrameFormatStandard::StereoLeft => "stereo_left",
            DaiFrameFormatStandard::StereoRight => "stereo_right",
            DaiFrameFormatStandard::Tdm1 => "tdm1",
            DaiFrameFormatStandard::Tdm2 => "tdm2",
            DaiFrameFormatStandard::Tdm3 => "tdm3",
        };
        f.write_str(s)
    }
}

impl From<fhaudio::DaiFrameFormatStandard> for DaiFrameFormatStandard {
    fn from(value: fhaudio::DaiFrameFormatStandard) -> Self {
        match value {
            fhaudio::DaiFrameFormatStandard::None => Self::None,
            fhaudio::DaiFrameFormatStandard::I2S => Self::I2S,
            fhaudio::DaiFrameFormatStandard::StereoLeft => Self::StereoLeft,
            fhaudio::DaiFrameFormatStandard::StereoRight => Self::StereoRight,
            fhaudio::DaiFrameFormatStandard::Tdm1 => Self::Tdm1,
            fhaudio::DaiFrameFormatStandard::Tdm2 => Self::Tdm2,
            fhaudio::DaiFrameFormatStandard::Tdm3 => Self::Tdm3,
        }
    }
}

impl From<DaiFrameFormatStandard> for fhaudio::DaiFrameFormatStandard {
    fn from(value: DaiFrameFormatStandard) -> Self {
        match value {
            DaiFrameFormatStandard::None => Self::None,
            DaiFrameFormatStandard::I2S => Self::I2S,
            DaiFrameFormatStandard::StereoLeft => Self::StereoLeft,
            DaiFrameFormatStandard::StereoRight => Self::StereoRight,
            DaiFrameFormatStandard::Tdm1 => Self::Tdm1,
            DaiFrameFormatStandard::Tdm2 => Self::Tdm2,
            DaiFrameFormatStandard::Tdm3 => Self::Tdm3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaiFrameFormatCustom {
    pub justification: DaiFrameFormatJustification,
    pub clocking: DaiFrameFormatClocking,
    pub frame_sync_sclks_offset: i8,
    pub frame_sync_size: u8,
}

impl Display for DaiFrameFormatCustom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{},{},{},{}",
            self.justification, self.clocking, self.frame_sync_sclks_offset, self.frame_sync_size
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaiFrameFormatJustification {
    Left,
    Right,
}

impl Display for DaiFrameFormatJustification {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DaiFrameFormatJustification::Left => "left_justified",
            DaiFrameFormatJustification::Right => "right_justified",
        };
        f.write_str(s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaiFrameFormatClocking {
    RaisingSclk,
    FallingSclk,
}

impl Display for DaiFrameFormatClocking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            DaiFrameFormatClocking::RaisingSclk => "raising_sclk",
            DaiFrameFormatClocking::FallingSclk => "falling_sclk",
        };
        f.write_str(s)
    }
}

impl From<DaiFrameFormatCustom> for fhaudio::DaiFrameFormatCustom {
    fn from(value: DaiFrameFormatCustom) -> Self {
        Self {
            left_justified: match value.justification {
                DaiFrameFormatJustification::Left => true,
                DaiFrameFormatJustification::Right => false,
            },
            sclk_on_raising: match value.clocking {
                DaiFrameFormatClocking::RaisingSclk => true,
                DaiFrameFormatClocking::FallingSclk => false,
            },
            frame_sync_sclks_offset: value.frame_sync_sclks_offset,
            frame_sync_size: value.frame_sync_size,
        }
    }
}

impl From<fhaudio::DaiFrameFormatCustom> for DaiFrameFormatCustom {
    fn from(value: fhaudio::DaiFrameFormatCustom) -> Self {
        Self {
            justification: if value.left_justified {
                DaiFrameFormatJustification::Left
            } else {
                DaiFrameFormatJustification::Right
            },
            clocking: if value.sclk_on_raising {
                DaiFrameFormatClocking::RaisingSclk
            } else {
                DaiFrameFormatClocking::FallingSclk
            },
            frame_sync_sclks_offset: value.frame_sync_sclks_offset,
            frame_sync_size: value.frame_sync_size,
        }
    }
}

#[cfg(test)]
pub mod test {
    use super::*;
    use test_case::test_case;

    #[test_case(DaiFrameFormat::Standard(DaiFrameFormatStandard::None), "none"; "standard: none")]
    #[test_case(DaiFrameFormat::Standard(DaiFrameFormatStandard::I2S), "i2s"; "standard: i2s")]
    #[test_case(
        DaiFrameFormat::Standard(DaiFrameFormatStandard::StereoLeft),
        "stereo_left";
        "standard: stereo_left"
    )]
    #[test_case(
        DaiFrameFormat::Standard(DaiFrameFormatStandard::StereoRight),
        "stereo_right";
        "standard: stereo_right"
    )]
    #[test_case(DaiFrameFormat::Standard(DaiFrameFormatStandard::Tdm1), "tdm1"; "standard: tdm1")]
    #[test_case(DaiFrameFormat::Standard(DaiFrameFormatStandard::Tdm2), "tdm2"; "standard: tdm2")]
    #[test_case(DaiFrameFormat::Standard(DaiFrameFormatStandard::Tdm3), "tdm3"; "standard: tdm3")]
    #[test_case(
        DaiFrameFormat::Custom(DaiFrameFormatCustom {
            justification: DaiFrameFormatJustification::Left,
            clocking: DaiFrameFormatClocking::RaisingSclk,
            frame_sync_sclks_offset: 0,
            frame_sync_size: 1,
        }),
        "custom:left_justified,raising_sclk,0,1";
        "custom 1"
    )]
    #[test_case(
        DaiFrameFormat::Custom(DaiFrameFormatCustom {
            justification: DaiFrameFormatJustification::Right,
            clocking: DaiFrameFormatClocking::FallingSclk,
            frame_sync_sclks_offset: -1,
            frame_sync_size: 0,
        }),
        "custom:right_justified,falling_sclk,-1,0";
        "custom 2"
    )]
    fn test_dai_frame_format_display(dai_frame_format: DaiFrameFormat, expected: &str) {
        assert_eq!(dai_frame_format.to_string(), expected);
    }
}
