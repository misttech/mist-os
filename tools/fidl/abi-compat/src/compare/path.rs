// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use flyweights::FlyStr;
use std::fmt::Display;

#[derive(PartialEq, PartialOrd, Eq, Ord, Debug, Clone)]
pub enum PathElement {
    Member(FlyStr, Option<FlyStr>),
    List(Option<FlyStr>),
}

impl Into<String> for &PathElement {
    fn into(self) -> String {
        use PathElement::*;
        match self {
            Member(name, _) => format!(".{name}"),
            List(_) => "[]".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd, Eq)]
pub struct Path {
    api_level: FlyStr,
    elements: Vec<PathElement>,
}
impl Path {
    #[cfg(test)]
    pub fn empty() -> Self {
        Self { api_level: FlyStr::new(""), elements: vec![] }
    }
    pub fn new(api_level: &FlyStr) -> Self {
        Self { api_level: api_level.clone(), elements: vec![] }
    }

    #[cfg(test)]
    pub fn with(&self, element: PathElement) -> Self {
        let mut elements = self.elements.clone();
        elements.push(element);
        Self { api_level: self.api_level.clone(), elements }
    }

    pub fn push(&mut self, element: PathElement) {
        self.elements.push(element);
    }
}

impl Path {
    pub fn api_level(&self) -> &str {
        self.api_level.as_str()
    }
    pub fn string(&self) -> String {
        self.elements.iter().fold("".to_owned(), |mut s, element| match element {
            PathElement::Member(name, _) => {
                if !s.is_empty() {
                    s.push_str(".")
                }
                s.push_str(name);
                s
            }
            PathElement::List(_) => {
                s.push_str("[]");
                s
            }
        })
    }
}

#[test]
fn test_path_string() {
    let p = Path { api_level: FlyStr::new(""), elements: vec![] };
    assert_eq!(p.string(), "");

    let p = p.with(PathElement::Member("foo".into(), None));
    assert_eq!(p.string(), "foo");

    let p = p.with(PathElement::Member("bar".into(), None));
    assert_eq!(p.string(), "foo.bar");

    let p = p.with(PathElement::List(None));
    assert_eq!(p.string(), "foo.bar[]");

    let p = p.with(PathElement::Member("baz".into(), None));
    assert_eq!(p.string(), "foo.bar[].baz");

    let p = p.with(PathElement::List(None));
    assert_eq!(p.string(), "foo.bar[].baz[]");

    let p = p.with(PathElement::List(None));
    assert_eq!(p.string(), "foo.bar[].baz[][]");
}

impl Display for Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.string())
    }
}
