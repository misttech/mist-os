// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use strum_macros::EnumString;
use thiserror::Error;

use std::fmt::Display;
use std::str::FromStr;

/// A feature that can be specified in a starnix kernel or container's configuration.
#[derive(Debug, Clone, Copy, PartialEq, EnumString, strum_macros::Display)]
#[strum(serialize_all = "snake_case")]
pub enum Feature {
    AndroidFdr,
    AndroidSerialno,
    AndroidBootreason,
    AspectRatio,
    Container,
    CustomArtifacts,
    Ashmem,
    Framebuffer,
    Gralloc,
    Kgsl,
    Magma,
    MagmaSupportedVendors,
    Nanohub,
    Fastrpc,
    NetstackMark,
    NetworkManager,
    Gfxstream,
    Bpf,
    EnableSuid,
    IoUring,
    ErrorOnFailedReboot,
    Perfetto,
    PerfettoProducer,
    RootfsRw,
    SelfProfile,
    Selinux,
    SelinuxTestSuite,
    TestData,
    Thermal,
    HvdcpOpti,
}

/// Error returned when a feature is not recognized.
#[derive(Debug, Error)]
#[error("unsupported feature: {0}")]
pub struct UnsupportedFeatureError(String);

impl Feature {
    /// Parses the name of a feature from a string.
    pub fn try_parse(s: &str) -> Result<Feature, UnsupportedFeatureError> {
        Feature::from_str(s).map_err(|_| UnsupportedFeatureError(s.to_string()))
    }

    /// Parses a feature and args from a string.
    pub fn try_parse_feature_and_args(
        s: &str,
    ) -> Result<(Feature, Option<String>), UnsupportedFeatureError> {
        let (raw_flag, raw_args) =
            s.split_once(':').map(|(f, a)| (f, Some(a.to_string()))).unwrap_or((s, None));
        Self::try_parse(raw_flag).map(|feature| (feature, raw_args))
    }
}

/// A feature together with any arguments that go along with it, if specified.
#[derive(Debug, Clone, PartialEq)]
pub struct FeatureAndArgs {
    /// The feature.
    pub feature: Feature,
    /// If specified, the (unparsed) arguments for the feature.
    pub raw_args: Option<String>,
}

impl FeatureAndArgs {
    /// Parses a feature and args from a string that separates them with `:`, e.g. "bpf:v2".
    ///
    /// If there is no `:` then the whole string is interpreted as the feature name.
    pub fn try_parse(s: &str) -> Result<FeatureAndArgs, UnsupportedFeatureError> {
        let (raw_flag, raw_args) =
            s.split_once(':').map(|(f, a)| (f, Some(a.to_string()))).unwrap_or((s, None));
        let feature = Feature::try_parse(raw_flag)?;
        Ok(FeatureAndArgs { feature, raw_args })
    }
}

impl Display for FeatureAndArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        let FeatureAndArgs { feature, raw_args } = self;
        match raw_args {
            None => feature.fmt(f),
            Some(raw_args) => format_args!("{feature}:{raw_args}").fmt(f),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn feature_serde() {
        for (feature, expected_str) in [
            (Feature::AndroidFdr, "android_fdr"),
            (Feature::AndroidSerialno, "android_serialno"),
            (Feature::AndroidBootreason, "android_bootreason"),
            (Feature::AspectRatio, "aspect_ratio"),
            (Feature::Container, "container"),
            (Feature::CustomArtifacts, "custom_artifacts"),
            (Feature::Ashmem, "ashmem"),
            (Feature::Framebuffer, "framebuffer"),
            (Feature::Gralloc, "gralloc"),
            (Feature::Kgsl, "kgsl"),
            (Feature::Magma, "magma"),
            (Feature::MagmaSupportedVendors, "magma_supported_vendors"),
            (Feature::Nanohub, "nanohub"),
            (Feature::NetstackMark, "netstack_mark"),
            (Feature::NetworkManager, "network_manager"),
            (Feature::Gfxstream, "gfxstream"),
            (Feature::Bpf, "bpf"),
            (Feature::EnableSuid, "enable_suid"),
            (Feature::IoUring, "io_uring"),
            (Feature::ErrorOnFailedReboot, "error_on_failed_reboot"),
            (Feature::Perfetto, "perfetto"),
            (Feature::PerfettoProducer, "perfetto_producer"),
            (Feature::RootfsRw, "rootfs_rw"),
            (Feature::SelfProfile, "self_profile"),
            (Feature::Selinux, "selinux"),
            (Feature::SelinuxTestSuite, "selinux_test_suite"),
            (Feature::TestData, "test_data"),
            (Feature::Thermal, "thermal"),
            (Feature::HvdcpOpti, "hvdcp_opti"),
        ] {
            let string = feature.to_string();
            assert_eq!(string.as_str(), expected_str);
            assert_eq!(Feature::try_parse(&string).expect("should parse"), feature);
        }
    }

    #[test]
    fn deserialize_feature_and_args() {
        let FeatureAndArgs { feature, raw_args } =
            FeatureAndArgs::try_parse("bpf:v2").expect("should parse successfully");
        assert_eq!(feature, Feature::Bpf);
        assert_eq!(raw_args.as_ref().expect("should be populated"), "v2");
    }
}
