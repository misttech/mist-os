// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Metadata for [time matrices][`TimeMatrix`].
//!
//! The [`DataSemantic`] of the [`Statistic`] determines which metadata types can be used to
//! annotate a [`TimeMatrix`]. For example, the [`Union`] statistic has a [`BitSet`] semantic that
//! requires [`BitSetMetadata`].
//!
//! [`BitSet`]: crate::experimental::series::BitSet
//! [`BitSetMetadata`]: crate::experimental::series::metadata::BitSetMetadata
//! [`DataSemantic`]: crate::experimental::series::DataSemantic
//! [`Statistic`]: crate::experimental::series::statistic::Statistic
//! [`TimeMatrix`]: crate::experimental::series::TimeMatrix
//! [`Union`]: crate::experimental::series::statistic::Union

use fuchsia_inspect::Node;
use itertools::Itertools;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};

pub trait Metadata {
    fn record(&self, node: &Node);
}

impl Metadata for () {
    fn record(&self, _: &Node) {}
}

/// A textual label for a bit in a [`BitSet`] aggregation.
///
/// [`BitSet`]: crate::experimental::series::BitSet
#[derive(Clone, Debug)]
pub struct BitLabel(Cow<'static, str>);

impl BitLabel {}

impl AsRef<str> for BitLabel {
    fn as_ref(&self) -> &str {
        self.0.as_ref()
    }
}

impl Display for BitLabel {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl From<Cow<'static, str>> for BitLabel {
    fn from(label: Cow<'static, str>) -> Self {
        BitLabel(label)
    }
}

impl From<&'static str> for BitLabel {
    fn from(label: &'static str) -> Self {
        BitLabel(label.into())
    }
}

impl From<String> for BitLabel {
    fn from(label: String) -> Self {
        BitLabel(label.into())
    }
}

// TODO(https://fxbug.dev/375475120): It is easiest to construct a `BitSetMap` inline with a
//                                    `Reactor`. This means that the mapping is most likely defined
//                                    far from the data type that represents the bit set. Staleness
//                                    is likely to occur if the mapping or data type change.
//
//                                    Provide an additional mechanism for defining this mapping
//                                    locally to the sampled data type.
/// A map from index to [`BitLabel`]s that indexes a [`BitSet`] aggregation.
///
/// A `BitSetMap` maps labels to particular bits in a bit set. These labels cannot change and are
/// recorded directly in the metadata for a [`TimeMatrix`].
///
/// [`BitLabel`]: crate::experimental::series::metadata::BitLabel
/// [`BitSet`]: crate::experimental::series::BitSet
/// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
#[derive(Clone, Debug)]
pub struct BitSetMap {
    labels: BTreeMap<usize, BitLabel>,
}

impl BitSetMap {
    pub fn from_ordered<I>(labels: I) -> Self
    where
        I: IntoIterator,
        I::Item: Into<BitLabel>,
    {
        BitSetMap { labels: labels.into_iter().map(Into::into).enumerate().collect() }
    }

    pub fn from_indexed<T, I>(labels: I) -> Self
    where
        T: Into<BitLabel>,
        I: IntoIterator<Item = (usize, T)>,
    {
        BitSetMap {
            labels: labels
                .into_iter()
                .unique_by(|(index, _)| *index)
                .map(|(index, label)| (index, label.into()))
                .collect(),
        }
    }

    fn record(&self, node: &Node) {
        node.record_child("index", |node| {
            for (index, label) in self
                .labels()
                .filter_map(|(index, label)| u64::try_from(*index).ok().map(|index| (index, label)))
            {
                // The index is used as the key, mapping from index to label in the Inspect tree.
                // Inspect sorts keys numerically, so there is no need to format the index.
                node.record_string(index.to_string(), label.as_ref());
            }
        });
    }

    pub fn labels(&self) -> impl '_ + Iterator<Item = (&usize, &BitLabel)> {
        self.labels.iter()
    }

    pub fn label(&self, index: usize) -> Option<&BitLabel> {
        self.labels.get(&index)
    }
}

/// A reference to an Inspect node that indexes a [`BitSet`] aggregation.
///
/// A `BitSetNode` provides a path to an Inspect node in which each key represents a bit index and
/// each value represents a label. Unlike [`BitSetMap`], this index is not tightly coupled to a
/// served [`TimeMatrix`] and the index may be dynamic.
///
/// Only the path to the Inspect node is recorded in the metadata for a [`TimeMatrix`].
///
/// [`BitSet`]: crate::experimental::series::BitSet
/// [`BitSetMap`]: crate::experimental::series::metadata::BitSetMap
/// [`TimeMatrix`]: crate::experimental::series::TimeMatrix
#[derive(Clone, Debug)]
pub struct BitSetNode {
    path: Cow<'static, str>,
}

impl BitSetNode {
    pub fn from_path(path: impl Into<Cow<'static, str>>) -> Self {
        BitSetNode { path: path.into() }
    }

    fn record(&self, node: &Node) {
        node.record_string("index_node_path", self.path.as_ref());
    }
}

/// Metadata for a [`BitSet`] aggregation that indexes bits with textual labels.
///
/// [`BitSet`]: crate::experimental::series::BitSet
#[derive(Clone, Debug)]
pub enum BitSetIndex {
    Map(BitSetMap),
    Node(BitSetNode),
}

impl From<BitSetMap> for BitSetIndex {
    fn from(metadata: BitSetMap) -> Self {
        BitSetIndex::Map(metadata)
    }
}

impl From<BitSetNode> for BitSetIndex {
    fn from(metadata: BitSetNode) -> Self {
        BitSetIndex::Node(metadata)
    }
}

impl Metadata for BitSetIndex {
    fn record(&self, node: &Node) {
        match self {
            BitSetIndex::Map(ref metadata) => metadata.record(node),
            BitSetIndex::Node(ref metadata) => metadata.record(node),
        }
    }
}
