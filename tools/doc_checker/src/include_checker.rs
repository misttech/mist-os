// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! include_checker implements the `DocCheck` trait  used to perform checks on the
//! files included using the << >> element.

use crate::checker::{DocCheck, DocCheckError};
use crate::md_element::Element;
use crate::DocCheckerArgs;
use anyhow::Result;
use async_trait::async_trait;
use lazy_static::lazy_static;
use pulldown_cmark::Tag;
use regex::Regex;
use std::path::PathBuf;

// path_help is a wrapper to allow mocking path checks
// exists. and is_dir.
cfg_if::cfg_if! {
    if #[cfg(test)] {
       use crate::mock_path_helper_module as path_helper;
    } else {
       use crate::path_helper_module as path_helper;
    }
}

lazy_static! {
    static ref INCLUDE_REGEX: Regex = Regex::new(r"<<\s?(.+?\.md)\s?>>").unwrap();
}

pub(crate) struct IncludeChecker {}

impl IncludeChecker {
    /// Checks for the included markdown files to be included. This is done by
    /// creating the string for the element and then searching using regular expression.
    /// The conversion to string is used because the tokenizer is inconsistent when handling
    /// << filename >>.
    fn check_for_include_str<'a>(
        &self,
        element: &'a Element<'_>,
    ) -> Result<Option<Vec<DocCheckError>>> {
        let contents = element.get_contents();

        if let Some(current_file_dir) = element.doc_line().file_name.parent() {
            for line in contents.lines() {
                let trimmed = line.trim();
                for cap in INCLUDE_REGEX.captures_iter(trimmed) {
                    let file_path = PathBuf::from(&cap[1]);
                    if file_path.is_absolute() {
                        return Ok(Some(vec![DocCheckError::new_error(
                            element.doc_line().line_num,
                            element.doc_line().file_name,
                            &format!(
                                "Included markdown file {:?} must be a relative path.",
                                file_path
                            ),
                        )]));
                    } else {
                        let included = current_file_dir.join(file_path);
                        if !path_helper::exists(&included) {
                            return Ok(Some(vec![DocCheckError::new_error(
                                element.doc_line().line_num,
                                element.doc_line().file_name,
                                &format!("Included markdown file {:?} not found.", included),
                            )]));
                        }
                    }
                }
            }
        } else {
            anyhow::bail!("Cannot get parent dir of {:?}", element.doc_line().file_name)
        }
        Ok(None)
    }
}

#[async_trait]
impl DocCheck for IncludeChecker {
    fn name(&self) -> &str {
        "IncludeChecker"
    }

    fn check<'a>(&mut self, element: &'a Element<'_>) -> Result<Option<Vec<DocCheckError>>> {
        match element {
            Element::Block(Tag::Paragraph, _, _) => self.check_for_include_str(element),
            _ => return Ok(None),
        }
    }

    async fn post_check(&self) -> Result<Option<Vec<DocCheckError>>> {
        // No post check for includes
        Ok(None)
    }
}

/// Called from main to register all the checks to preform which are implemented in this module.
pub(crate) fn register_markdown_checks(_: &DocCheckerArgs) -> Result<Vec<Box<dyn DocCheck>>> {
    let checker = IncludeChecker {};
    Ok(vec![Box::new(checker)])
}

#[cfg(test)]
mod tests {
    use crate::md_element::DocContext;

    use super::*;

    #[test]
    fn test_non_matching() -> Result<()> {
        let mut checker = IncludeChecker {};

        let data = [
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                "non-markdown file OK to link to docs [non-source](https://fuchsia.googlesource.com/fuchsia/+/refs/heads/main/docs/OWNERS)",
            ),
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                "have 2 << but no close",
            ),
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                "have  << but no close\n on the same line>>",
            ),
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                "have a non-markdown file name  <<you name here>>",
            ),
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                "spaces   < <a-file.md> >",
            ),
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                "spaces   <<a-file.md> >",
            ),
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                "spaces   < <a-file.md>>",
            ),
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                "exists <<a-file.md>>",
            ),
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                "\n```md\nThis is a sample in codeblock to\n<<missing.md>>\n```\n",
            ),
        ];

        for ctx in data {
            for ele in ctx {
                let errors = checker.check(&ele)?;
                assert!(errors.is_none(), "Expected no errors got {:?}", errors);
            }
        }
        Ok(())
    }

    #[test]
    fn test_errors() -> Result<()> {
        let mut checker = IncludeChecker {};

        let data = [
            (
                DocContext::new(PathBuf::from("/docs/README.md"), "does not exist <<missing.md>>"),
                vec![DocCheckError::new_error(
                    1,
                    PathBuf::from("/docs/README.md"),
                    "Included markdown file \"/docs/missing.md\" not found.",
                )],
            ),
            (
                DocContext::new(
                    PathBuf::from("/docs/README.md"),
                    " no absolute\" <</docs/README.md>>",
                ),
                vec![DocCheckError::new_error(
                    1,
                    PathBuf::from("/docs/README.md"),
                    "Included markdown file \"/docs/README.md\" must be a relative path.",
                )],
            ),
        ];

        for (ctx, expected) in data {
            for ele in ctx {
                let errors = checker.check(&ele)?;
                let mut expected_iter = expected.iter();
                if let Some(actual_errors) = errors {
                    for actual in actual_errors {
                        if let Some(expected) = expected_iter.next() {
                            assert_eq!(&actual, expected);
                        } else {
                            panic!("Got unexpected error returned: {:?}", actual);
                        }
                    }
                    let unused_errors: Vec<&DocCheckError> = expected_iter.collect();
                    if !unused_errors.is_empty() {
                        panic!("Expected more errors: {:?}", unused_errors);
                    }
                }
            }
        }
        Ok(())
    }
}
