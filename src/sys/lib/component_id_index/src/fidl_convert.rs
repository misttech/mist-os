// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{Index, InstanceIdEntry};
use fidl_fuchsia_component_internal as fcomponent_internal;
use moniker::{Moniker, MonikerBase, MonikerError};
use std::convert::TryFrom;
use thiserror::Error;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Error, Clone, Debug, PartialEq)]
pub enum FidlConversionError {
    #[error("Missing instances")]
    MissingInstances,
    #[error("Moniker error")]
    MonikerError(#[from] MonikerError),
}

// This converter translates between different encodings but does not do any semantic validation.
// To construct a validated Index, use a Index::from_*() constructor along with this converter.
impl TryFrom<fcomponent_internal::ComponentIdIndex> for Index {
    type Error = FidlConversionError;

    fn try_from(index: fcomponent_internal::ComponentIdIndex) -> Result<Self, Self::Error> {
        let mut instances: Vec<InstanceIdEntry> = vec![];
        for entry in index.instances.ok_or_else(|| FidlConversionError::MissingInstances)? {
            instances.push(InstanceIdEntry {
                instance_id: entry.instance_id,
                moniker: entry
                    .moniker
                    .map(|moniker_str| Moniker::parse_str(&moniker_str))
                    .transpose()?,
            });
        }
        Ok(Index { instances })
    }
}

impl From<Index> for fcomponent_internal::ComponentIdIndex {
    fn from(index: Index) -> Self {
        fcomponent_internal::ComponentIdIndex {
            instances: Some(
                index
                    .instances
                    .into_iter()
                    .map(|entry| fcomponent_internal::InstanceIdEntry {
                        instance_id: entry.instance_id,
                        moniker: entry.moniker.map(|abs_moniker| abs_moniker.to_string()),
                        ..Default::default()
                    })
                    .collect(),
            ),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fidl_native_translation() {
        let fidl_index = fcomponent_internal::ComponentIdIndex {
            instances: Some(vec![fcomponent_internal::InstanceIdEntry {
                instance_id: Some("abc".to_string()),
                moniker: Some("a/b/c".to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        };

        let native_index = Index {
            instances: vec![InstanceIdEntry {
                instance_id: Some("abc".to_string()),
                moniker: Some(Moniker::parse_str("a/b/c").unwrap()),
            }],
        };

        assert_eq!(native_index, Index::try_from(fidl_index.clone()).unwrap());
        assert_eq!(fidl_index, fcomponent_internal::ComponentIdIndex::from(native_index));
    }

    #[test]
    fn missing_instances() {
        assert_eq!(
            Some(FidlConversionError::MissingInstances),
            Index::try_from(fcomponent_internal::ComponentIdIndex { ..Default::default() }).err()
        );
    }
}
