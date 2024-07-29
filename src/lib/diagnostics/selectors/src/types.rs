// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_diagnostics as fdiagnostics;
use std::borrow::Cow;
use std::fmt::Debug;

#[derive(Debug, Eq, PartialEq)]
pub enum Segment<'a> {
    ExactMatch(Cow<'a, str>),
    Pattern(&'a str),
}

fn contains_unescaped_wildcard(s: &str) -> bool {
    let mut iter = s.chars();
    while let Some(c) = iter.next() {
        match c {
            '*' => return true,
            '\\' => {
                // skip escaped characters
                let _ = iter.next();
            }
            _ => {}
        }
    }
    false
}

impl<'a> Into<Segment<'a>> for &'a str {
    fn into(self) -> Segment<'a> {
        if contains_unescaped_wildcard(self) {
            return Segment::Pattern(self);
        }
        if !self.contains('\\') {
            return Segment::ExactMatch(Cow::from(self));
        }
        let mut result = String::with_capacity(self.len());
        let mut iter = self.chars();
        while let Some(c) = iter.next() {
            match c {
                '\\' => {
                    // push unescaped character since we are constructing an exact match.
                    if let Some(c) = iter.next() {
                        result.push(c);
                    }
                }
                c => result.push(c),
            }
        }
        Segment::ExactMatch(Cow::from(result))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum TreeNames<'a> {
    Some(Vec<Cow<'a, str>>),
    All,
}

impl<'a> Into<TreeNames<'a>> for Vec<&'a str> {
    fn into(self) -> TreeNames<'a> {
        let mut payload = vec![];
        for name in self {
            if !name.contains('\\') {
                payload.push(Cow::Borrowed(name));
                continue;
            }
            let mut result = String::with_capacity(name.len());
            let mut iter = name.chars();
            while let Some(c) = iter.next() {
                match c {
                    '\\' => {
                        // push unescaped character since we are constructing an exact match.
                        if let Some(c) = iter.next() {
                            result.push(c);
                        }
                    }
                    c => result.push(c),
                }
            }
            payload.push(Cow::Owned(result));
        }
        TreeNames::Some(payload)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct TreeSelector<'a> {
    pub node: Vec<Segment<'a>>,
    pub property: Option<Segment<'a>>,
    pub tree_names: Option<TreeNames<'a>>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct ComponentSelector<'a> {
    pub segments: Vec<Segment<'a>>,
}

#[derive(Debug, Eq, PartialEq)]
pub struct Selector<'a> {
    pub component: ComponentSelector<'a>,
    pub tree: TreeSelector<'a>,
}

impl Into<fdiagnostics::Selector> for Selector<'_> {
    fn into(mut self) -> fdiagnostics::Selector {
        let tree_names = self.tree.tree_names.take();
        fdiagnostics::Selector {
            component_selector: Some(self.component.into()),
            tree_selector: Some(self.tree.into()),
            tree_names: tree_names.map(|names| names.into()),
            ..Default::default()
        }
    }
}

impl Into<fdiagnostics::ComponentSelector> for ComponentSelector<'_> {
    fn into(self) -> fdiagnostics::ComponentSelector {
        fdiagnostics::ComponentSelector {
            moniker_segments: Some(
                self.segments.into_iter().map(|segment| segment.into()).collect(),
            ),
            ..Default::default()
        }
    }
}

impl Into<fdiagnostics::TreeSelector> for TreeSelector<'_> {
    fn into(self) -> fdiagnostics::TreeSelector {
        let node_path = self.node.into_iter().map(|s| s.into()).collect();
        match self.property {
            None => fdiagnostics::TreeSelector::SubtreeSelector(fdiagnostics::SubtreeSelector {
                node_path,
            }),
            Some(property) => {
                fdiagnostics::TreeSelector::PropertySelector(fdiagnostics::PropertySelector {
                    node_path,
                    target_properties: property.into(),
                })
            }
        }
    }
}

impl Into<fdiagnostics::TreeNames> for TreeNames<'_> {
    fn into(self) -> fdiagnostics::TreeNames {
        match self {
            Self::All => fdiagnostics::TreeNames::All(fdiagnostics::All {}),
            Self::Some(names) => {
                fdiagnostics::TreeNames::Some(names.iter().map(|n| n.to_string()).collect())
            }
        }
    }
}

impl Into<fdiagnostics::StringSelector> for Segment<'_> {
    fn into(self) -> fdiagnostics::StringSelector {
        match self {
            Segment::ExactMatch(s) => fdiagnostics::StringSelector::ExactMatch(s.into_owned()),
            Segment::Pattern(s) => fdiagnostics::StringSelector::StringPattern(s.to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn convert_string_to_segment() {
        assert_eq!(Segment::ExactMatch(Cow::Borrowed("abc")), "abc".into());
        assert_eq!(Segment::Pattern("a*c"), "a*c".into());
        assert_eq!(Segment::ExactMatch(Cow::Owned("ac*".into())), "ac\\*".into());
        assert_eq!(Segment::Pattern("a\\*c*"), "a\\*c*".into());
    }
}
