// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{cml, cml::features::FeatureSet, cml::ProtocolRequirements, error::Error, util},
    std::path::Path,
};

/// Validates that all given manifest files are correct cml.
///
/// Returns an Err() if any file is not valid or Ok(()) if all files are valid.
pub fn validate<P: AsRef<Path>>(
    files: &[P],
    features: &FeatureSet,
    protocol_requirements: ProtocolRequirements<'_>,
) -> Result<(), Error> {
    if files.is_empty() {
        return Err(Error::invalid_args("No files provided"));
    }

    for file in files {
        let file = file.as_ref();
        let document = util::read_cml(file)?;
        cml::compile(
            &document,
            cml::CompileOptions::new()
                .file(&file)
                .features(features)
                .protocol_requirements(protocol_requirements.clone()),
        )?;
    }
    Ok(())
}
