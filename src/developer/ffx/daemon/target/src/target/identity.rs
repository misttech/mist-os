// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[derive(Clone, Hash)] // Do not implement PartialEq and Eq
pub struct Identity {
    // Always present. Identities cannot be empty.
    ident: Ident,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("name", &self.name())
            .field("serial", &self.serial())
            .finish()
    }
}

// Structured to ensure empty identities cannot be constructed.
// Otherwise many invariants are broken.
#[derive(Clone, Debug, Hash)]
enum Ident {
    Name { name: String, serial: Option<String> },
    Serial { name: Option<String>, serial: String },
}

impl Ident {
    fn into_all(self) -> Parts {
        match self {
            Self::Name { name, serial } => Parts { name: Some(name), serial },
            Self::Serial { name, serial } => Parts { name, serial: Some(serial) },
        }
    }

    fn name(&self) -> Option<&str> {
        match self {
            Self::Name { name, .. } => Some(name),
            Self::Serial { name, .. } => name.as_deref(),
        }
    }

    fn serial(&self) -> Option<&str> {
        match self {
            Self::Name { serial, .. } => serial.as_deref(),
            Self::Serial { serial, .. } => Some(serial),
        }
    }
}

#[derive(Clone, Debug, Default, Hash)]
struct Parts {
    name: Option<String>,
    serial: Option<String>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IdentityCmp {
    Eq,
    Intersects,
    Contains,
}

impl Identity {
    pub fn from_name(name: impl Into<String>) -> Self {
        Self { ident: Ident::Name { name: name.into(), serial: None } }
    }

    pub fn from_serial(serial: impl Into<String>) -> Self {
        Self { ident: Ident::Serial { name: None, serial: serial.into() } }
    }

    pub fn try_from_name_serial<N, S>(name: Option<N>, serial: Option<S>) -> Option<Self>
    where
        N: Into<String>,
        S: Into<String>,
    {
        [name.map(Into::into).map(Self::from_name), serial.map(Into::into).map(Self::from_serial)]
            .into_iter()
            .flatten()
            .reduce(Self::join_with)
    }

    pub fn name(&self) -> Option<&str> {
        self.ident.name()
    }

    pub fn serial(&self) -> Option<&str> {
        self.ident.serial()
    }

    pub fn cmp_to(&self, other: &Self) -> Option<IdentityCmp> {
        if std::ptr::eq(self, other) {
            return Some(IdentityCmp::Eq);
        }

        // Mismatch, exact match, one known, unknown
        let name = Self::check_field(self.name(), other.name())?;
        let serial = Self::check_field(self.serial(), other.serial())?;

        // Unknown, e.g. [name] x [serial] = []
        if !(name || serial) {
            return None;
        }

        // Exact match
        if name && serial {
            return Some(IdentityCmp::Eq);
        }

        // Any information that only `other` knows?
        if self.name().is_none() && other.name().is_some() {
            // [serial] x [name, serial]
            return Some(IdentityCmp::Intersects);
        }

        if self.serial().is_none() && other.serial().is_some() {
            // [name] x [name, serial]
            return Some(IdentityCmp::Intersects);
        }

        // Otherwise `other` has less information
        // e.g. [name, serial] x [name]
        Some(IdentityCmp::Contains)
    }

    /// Returns whether this identity refers to the same object as `other`.
    ///
    /// Returns false if not determinable or determined not to overlap.
    pub fn is_same(&self, other: &Self) -> bool {
        self.cmp_to(other).is_some()
    }

    /// Returns whether this identity is a subset of `other`.
    pub fn intersects(&self, other: &Self) -> bool {
        self.cmp_to(other) == Some(IdentityCmp::Intersects)
    }

    /// Joins the knowledge of two identities together.
    ///
    /// Panics on field conflict. Use `Self::is_same` to check for conflicts first.
    pub fn join(&mut self, other: Self) {
        // Decompose `other` into its parts.
        let Parts { mut name, mut serial } = other.ident.into_all();

        // Filter out any fields that match across both identities.
        name = name.filter(|s| self.name() != Some(s));
        serial = serial.filter(|s| self.serial() != Some(s));

        assert!(
            name.is_none() || self.name().is_none(),
            "name field conflict: {:?} != {:?}",
            self.name(),
            name
        );
        assert!(
            serial.is_none() || self.serial().is_none(),
            "serial field conflict: {:?} != {:?}",
            self.serial(),
            serial
        );

        match (&mut self.ident, name, serial) {
            (Ident::Name { name: _, serial: self_serial }, _, Some(serial)) => {
                *self_serial = Some(serial);
            }
            (Ident::Serial { name: self_name, serial: _ }, Some(name), _) => {
                *self_name = Some(name);
            }
            _ => {}
        }
    }

    pub fn join_with(mut self, other: Self) -> Self {
        self.join(other);
        self
    }

    // Checks fields for conflicts and ambiguity
    fn check_field<T>(a: Option<T>, b: Option<T>) -> Option<bool>
    where
        T: Eq,
    {
        match (a, b) {
            (Some(a), Some(b)) if a == b => Some(true),
            (Some(..), Some(..)) => None,
            // Unknown overlap
            _ => Some(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmp_fails_on_conflict() {
        let node_a = Identity::from_name("node_a");
        let node_b = Identity::from_name("node_b");
        assert_eq!(node_a.cmp_to(&node_b), None);

        let serial_a = Identity::from_serial("serial_a");
        let serial_b = Identity::from_serial("serial_b");
        assert_eq!(serial_a.cmp_to(&serial_b), None);

        assert_eq!(node_a.join_with(serial_a).cmp_to(&node_b.join_with(serial_b)), None);
    }

    #[test]
    fn cmp_fails_when_ambiguous() {
        let node = Identity::from_name("node");
        let serial = Identity::from_serial("serial");

        assert_eq!(serial.cmp_to(&node), None);
    }

    #[test]
    fn cmp_handles_intersection() {
        let serial = Identity::from_serial("serial");
        let node = Identity::from_name("node").join_with(serial.clone());

        assert_eq!(serial.cmp_to(&node), Some(IdentityCmp::Intersects));

        let node = Identity::from_name("node");
        let serial = Identity::from_serial("serial").join_with(node.clone());

        assert_eq!(node.cmp_to(&serial), Some(IdentityCmp::Intersects));
    }

    #[test]
    fn cmp_handles_contains() {
        let serial = Identity::from_serial("serial");
        let node = Identity::from_name("node").join_with(serial.clone());

        assert_eq!(node.cmp_to(&serial), Some(IdentityCmp::Contains));

        let node = Identity::from_name("node");
        let serial = Identity::from_serial("serial").join_with(node.clone());

        assert_eq!(serial.cmp_to(&node), Some(IdentityCmp::Contains));
    }

    #[test]
    fn cmp_handles_eq() {
        let node = Identity::from_name("node");
        let serial = Identity::from_serial("serial");

        let a = serial.clone().join_with(node.clone());
        let b = node.clone().join_with(serial.clone());

        assert_eq!(a.cmp_to(&b), Some(IdentityCmp::Eq));
        assert_eq!(b.cmp_to(&a), Some(IdentityCmp::Eq));
    }
}
