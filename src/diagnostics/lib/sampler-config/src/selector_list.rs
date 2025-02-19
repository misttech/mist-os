// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_fuchsia_diagnostics::{Selector, StringSelector, TreeNames};
use selectors::FastError;
use serde::{Deserialize, Deserializer};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use thiserror::Error;

// SelectorList and StringList are adapted from SelectorEntry in
// src/diagnostics/lib/triage/src/config.rs

/// A selector entry in the configuration file is either a single string
/// or a vector of string selectors. Either case is converted to a vector
/// with at least one element.
///
/// Each element is optional so selectors can be removed when they're
/// known not to be needed. If one selector matches data, the others are
/// removed. After an upload_once is uploaded, all selectors are removed.
/// On initial parse, all elements will be Some<_>.
#[derive(Clone, Debug, PartialEq)]
pub struct SelectorList(Vec<Option<ParsedSelector>>);

impl<I: IntoIterator<Item = Option<ParsedSelector>>> From<I> for SelectorList {
    fn from(list: I) -> Self {
        SelectorList(list.into_iter().collect())
    }
}

impl std::ops::Deref for SelectorList {
    type Target = Vec<Option<ParsedSelector>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for SelectorList {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

/// ParsedSelector stores the information Sampler needs to use the selector.
// TODO(https://fxbug.dev/42168860) - this could be more memory-efficient by using slices into the string.
#[derive(Clone, Debug)]
pub struct ParsedSelector {
    /// The original string, needed to initialize the ArchiveAccessor
    pub selector_string: String,
    /// The parsed selector, needed to fetch the value out of the returned hierarchy
    pub selector: Selector,
    /// How many times this selector has found and uploaded data
    upload_count: Arc<AtomicU64>,
}

impl ParsedSelector {
    pub fn increment_upload_count(&self) {
        self.upload_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn get_upload_count(&self) -> u64 {
        self.upload_count.load(Ordering::Relaxed)
    }
}

impl PartialEq for ParsedSelector {
    fn eq(&self, other: &Self) -> bool {
        self.selector_string == other.selector_string
            && self.selector == other.selector
            && self.upload_count.load(Ordering::Relaxed)
                == other.upload_count.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("wildcarded component not allowlisted: '{0}'")]
    InvalidWildcardedSelector(String),

    #[error(transparent)]
    ParseSelector(#[from] selectors::Error),
}

struct WildcardRestriction {
    segments: Vec<StringSelector>,
    must_have_tree_name: bool,
}

static WILDCARD_RESTRICTIONS: LazyLock<Vec<WildcardRestriction>> = LazyLock::new(|| {
    vec![
        WildcardRestriction {
            segments: vec![
                StringSelector::ExactMatch("core".into()),
                StringSelector::ExactMatch("bluetooth-core".into()),
                StringSelector::StringPattern("bt-host-collection:bt-host_*".into()),
            ],
            must_have_tree_name: false,
        },
        WildcardRestriction {
            segments: vec![
                StringSelector::ExactMatch("bootstrap".into()),
                StringSelector::StringPattern("*-drivers:*".into()),
            ],
            must_have_tree_name: true,
        },
    ]
});

// `selector` must be validated.
fn verify_wildcard_restrictions(selector: &Selector, raw_selector: &str) -> Result<(), Error> {
    // Safety: assuming that the selector was parsed by selectors::parse_selectors, it has
    // been validated, and these unwraps are safe
    let actual_segments =
        selector.component_selector.as_ref().unwrap().moniker_segments.as_ref().unwrap();
    if !actual_segments.iter().any(|segment| matches!(segment, StringSelector::StringPattern(_))) {
        return Ok(());
    }
    for restriction in &*WILDCARD_RESTRICTIONS {
        if restriction.segments.len() != actual_segments.len() {
            continue;
        }
        if restriction
            .segments
            .iter()
            .zip(actual_segments.iter())
            .any(|(expected_segment, actual_segment)| expected_segment != actual_segment)
        {
            continue;
        }
        if restriction.must_have_tree_name {
            let Some(TreeNames::Some(_)) = selector.tree_names else {
                return Err(Error::InvalidWildcardedSelector(raw_selector.to_string()));
            };
        }
        return Ok(());
    }
    Err(Error::InvalidWildcardedSelector(raw_selector.to_string()))
}

pub(crate) fn parse_selector(selector_str: &str) -> Result<ParsedSelector, Error> {
    let selector = selectors::parse_selector::<FastError>(selector_str)?;
    verify_wildcard_restrictions(&selector, selector_str)?;
    Ok(ParsedSelector {
        selector,
        selector_string: selector_str.to_string(),
        upload_count: Arc::new(AtomicU64::new(0)),
    })
}

impl<'de> Deserialize<'de> for SelectorList {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct SelectorVec(std::marker::PhantomData<Vec<Option<ParsedSelector>>>);

        impl<'de> serde::de::Visitor<'de> for SelectorVec {
            type Value = Vec<Option<ParsedSelector>>;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("either a single selector or an array of selectors")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(vec![Some(parse_selector(value).map_err(E::custom)?)])
            }

            fn visit_seq<A>(self, mut value: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                use serde::de::Error;

                let mut out = vec![];
                while let Some(s) = value.next_element::<String>()? {
                    out.push(Some(parse_selector(&s).map_err(A::Error::custom)?));
                }
                if out.is_empty() {
                    Err(A::Error::invalid_length(0, &"expected at least one selector"))
                } else {
                    Ok(out)
                }
            }
        }

        Ok(SelectorList::from(d.deserialize_any(SelectorVec(std::marker::PhantomData))?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Error;
    use assert_matches::assert_matches;
    use fidl_fuchsia_diagnostics::TreeSelector;

    fn require_string(data: &StringSelector, required: &str) {
        match data {
            StringSelector::ExactMatch(string) => assert_eq!(string, required),
            _ => unreachable!("Expected an exact match"),
        }
    }

    fn require_strings(data: &[StringSelector], required: Vec<&str>) {
        assert_eq!(data.len(), required.len());
        for (data, required) in data.iter().zip(required.iter()) {
            require_string(data, required);
        }
    }

    #[fuchsia::test]
    fn parse_valid_single_selector() -> Result<(), Error> {
        let json = "\"core/foo:root/branch:leaf\"";
        let selectors: SelectorList = serde_json5::from_str(json)?;
        assert_eq!(selectors.len(), 1);
        let ParsedSelector { selector_string, selector, .. } = selectors[0].as_ref().unwrap();
        assert_eq!(selector_string, "core/foo:root/branch:leaf");
        match &selector.tree_selector {
            Some(TreeSelector::PropertySelector(selector)) => {
                require_strings(&selector.node_path, vec!["root", "branch"]);
                require_string(&selector.target_properties, "leaf");
            }
            _ => unreachable!("Expected a property selector"),
        }
        Ok(())
    }

    #[fuchsia::test]
    fn parse_valid_multiple_selectors() -> Result<(), Error> {
        let json = "[ \"core/foo:root/branch:leaf\", \"core/bar:root/twig:leaf\"]";
        let selectors: SelectorList = serde_json5::from_str(json)?;
        assert_eq!(selectors.len(), 2);
        let ParsedSelector { selector_string, selector, .. } = selectors[0].as_ref().unwrap();
        assert_eq!(selector_string, "core/foo:root/branch:leaf");
        match &selector.tree_selector {
            Some(TreeSelector::PropertySelector(selector)) => {
                require_strings(&selector.node_path, vec!["root", "branch"]);
                require_string(&selector.target_properties, "leaf");
            }
            _ => unreachable!("Expected a property selector"),
        }
        let ParsedSelector { selector_string, selector, .. } = selectors[1].as_ref().unwrap();
        assert_eq!(selector_string, "core/bar:root/twig:leaf");
        match &selector.tree_selector {
            Some(TreeSelector::PropertySelector(selector)) => {
                require_strings(&selector.node_path, vec!["root", "twig"]);
                require_string(&selector.target_properties, "leaf");
            }
            _ => unreachable!("Expected a property selector"),
        }
        Ok(())
    }

    #[fuchsia::test]
    fn refuse_invalid_selectors() {
        let bad_selector = "\"core/foo:wrong:root/branch:leaf\"";
        let not_string = "42";
        let bad_list = "[ \"core/foo:root/branch:leaf\", \"core/bar:wrong:root/twig:leaf\"]";
        serde_json5::from_str::<SelectorList>(bad_selector).expect_err("this should fail");
        serde_json5::from_str::<SelectorList>(not_string).expect_err("this should fail");
        serde_json5::from_str::<SelectorList>(bad_list).expect_err("this should fail");
    }

    #[fuchsia::test]
    fn wild_card_selectors() {
        let good_selector = r#"["bootstrap/*-drivers\\:*:[name=fvm]root:field"]"#;
        assert_matches!(serde_json5::from_str::<SelectorList>(good_selector), Ok(_));

        let good_selector = r#"["core/bluetooth-core/bt-host-collection\\:bt-host_*:root:field"]"#;
        assert_matches!(serde_json5::from_str::<SelectorList>(good_selector), Ok(_));

        let bad_selector = r#"["not_bootstrap/*-drivers\\:*:[name=fvm]root:field"]"#;
        assert_matches!(serde_json5::from_str::<SelectorList>(bad_selector), Err(_));

        let not_exact_collection_match = r#"["bootstrap/*-drivers*:[name=fvm]root:field"]"#;
        assert_matches!(serde_json5::from_str::<SelectorList>(not_exact_collection_match), Err(_));

        let missing_filter = r#"["not_bootstrap/*-drivers\\:*:root:field"]"#;
        assert_matches!(serde_json5::from_str::<SelectorList>(missing_filter), Err(_));
    }
}
