// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub type ComponentName = String;
pub type CapabilityName = String;

#[derive(Clone, Debug)]
pub enum Availability {
    Optional,
    Required,
    SameAsTarget,
    Transitional,
}

#[derive(Clone, Debug)]
pub enum Ref {
    // A standin for someplace where you can always get any capability.
    // In reality this would be "self" or "framework".
    Hammerspace,
    Parent,
    Child(ComponentName),
}

#[derive(Clone, Debug)]
pub struct Use {
    pub name: CapabilityName,
    pub from: Ref,
    pub availability: Availability,
}

#[derive(Clone, Debug)]
pub struct Offer {
    pub name: CapabilityName,
    pub from: Ref,
    pub to: Ref,
    pub availability: Availability,
}

#[derive(Clone, Debug)]
pub struct Expose {
    pub name: CapabilityName,
    pub from: Ref,
    pub availability: Availability,
}

#[derive(Clone, Debug)]
pub struct Child {
    pub name: ComponentName,
}

#[derive(Clone, Debug)]
pub struct Component {
    pub uses: Vec<Use>,
    pub offers: Vec<Offer>,
    pub exposes: Vec<Expose>,
    pub children: Vec<Child>,
}
