// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! md_parser defines the traits and structs used to parse markdown files into elements that can be
//! checked.

use crate::{parser, DocLine};
pub use pulldown_cmark::{CowStr, LinkType, Options, Parser, Tag};
use std::{collections::HashMap, fmt::Debug, ops::Range, path::PathBuf};

/// Element is a high level construct which collects the low level Tag objects into
/// a single element. This removes the need for checks to deal with stateful processing of the event stream.
#[derive(Debug, PartialEq)]
pub enum Element<'a> {
    /// A generic block of elements. The tuple is (block_type, elements, doc_line).
    Block(Tag<'a>, Vec<Element<'a>>, DocLine),
    /// An inline `code` string.
    Code(CowStr<'a>, DocLine),
    /// A ``` code fence
    CodeBlock(CowStr<'a>, Vec<Element<'a>>, DocLine),
    /// A footnote reference
    /// TODO: I have not seen one of these in our docs.
    FootnoteReference(CowStr<'a>, DocLine),
    /// Hard break newline.
    HardBreak(DocLine),
    /// HTML
    Html(CowStr<'a>, DocLine),
    /// Image block. (link_type, image_url, title, elements, doc_line)
    Image(LinkType, CowStr<'a>, CowStr<'a>, Vec<Element<'a>>, DocLine),
    /// Link block.  (link_type, link_url, title, elements, doc_line)
    Link(LinkType, CowStr<'a>, CowStr<'a>, Vec<Element<'a>>, DocLine),
    /// List (starting number or None, items, doc_line)
    List(Option<u64>, Vec<Element<'a>>, DocLine),
    /// Softbreak newline.
    SoftBreak(DocLine),
    //HR Rule
    Rule(DocLine),
    /// TaskList. bool indicating checked.
    TaskListMarker(bool, DocLine),
    /// Text
    Text(CowStr<'a>, DocLine),
}

#[allow(dead_code)]
impl<'a> Element<'a> {
    pub fn doc_line(&self) -> DocLine {
        let doc_line = match self {
            Element::Block(_, _, doc_line) => doc_line,
            Element::Code(_, doc_line) => doc_line,
            Element::CodeBlock(_, _, doc_line) => doc_line,
            Element::FootnoteReference(_, doc_line) => doc_line,
            Element::HardBreak(doc_line) => doc_line,
            Element::Html(_, doc_line) => doc_line,
            Element::Image(_, _, _, _, doc_line) => doc_line,
            Element::Link(_, _, _, _, doc_line) => doc_line,
            Element::List(_, _, doc_line) => doc_line,
            Element::Rule(doc_line) => doc_line,
            Element::SoftBreak(doc_line) => doc_line,
            Element::TaskListMarker(_, doc_line) => doc_line,
            Element::Text(_, doc_line) => doc_line,
        };
        doc_line.clone()
    }

    fn doc_line_mut(&mut self) -> &mut DocLine {
        match self {
            Element::Block(_, _, ref mut doc_line) => doc_line,
            Element::Code(_, ref mut doc_line) => doc_line,
            Element::CodeBlock(_, _, ref mut doc_line) => doc_line,
            Element::FootnoteReference(_, ref mut doc_line) => doc_line,
            Element::HardBreak(ref mut doc_line) => doc_line,
            Element::Html(_, ref mut doc_line) => doc_line,
            Element::Image(_, _, _, _, ref mut doc_line) => doc_line,
            Element::Link(_, _, _, _, ref mut doc_line) => doc_line,
            Element::List(_, _, ref mut doc_line) => doc_line,
            Element::Rule(ref mut doc_line) => doc_line,
            Element::SoftBreak(ref mut doc_line) => doc_line,
            Element::TaskListMarker(_, ref mut doc_line) => doc_line,
            Element::Text(_, ref mut doc_line) => doc_line,
        }
    }

    pub fn get_contents(&self) -> String {
        match self {
            Element::Block(_, elements, _) => {
                elements.iter().map(|e| e.get_contents()).collect::<Vec<String>>().join("")
            }
            Element::Code(code, _) => code.to_string(),
            Element::CodeBlock(code, elements, _) => {
                let mut parts = vec![code.to_string()];
                parts.extend(elements.iter().map(|e| e.get_contents()));
                parts.join(" ")
            }
            Element::FootnoteReference(footnote, _) => footnote.to_string(),
            Element::HardBreak(_) => "\n".to_string(),
            Element::Html(html, _) => html.to_string(),
            Element::Image(_, _, title, elements, _) => {
                let mut parts = vec![title.to_string()];
                parts.extend(elements.iter().map(|e| e.get_contents()));
                parts.join(" ")
            }
            Element::Link(_, _, title, elements, _) => {
                let mut parts = vec![title.to_string()];
                parts.extend(elements.iter().map(|e| e.get_contents()));
                parts.join(" ")
            }
            Element::List(_, items, _) => {
                items.iter().map(|e| e.get_contents()).collect::<Vec<String>>().join(" ")
            }
            Element::Rule(_) => "".to_string(),
            Element::SoftBreak(_) => " ".to_string(),
            Element::TaskListMarker(_, _) => " ".to_string(),
            Element::Text(text, _) => text.to_string(),
        }
    }

    pub fn get_links(&self) -> Option<Vec<&Element<'a>>> {
        match self {
            Element::Block(_, elements, _) => {
                let links: Vec<&Element<'a>> =
                    elements.iter().filter_map(|e| e.get_links()).flatten().collect();
                if !links.is_empty() {
                    Some(links)
                } else {
                    None
                }
            }
            Element::Code(_, _) => None,
            Element::CodeBlock(_, elements, _) => {
                let links: Vec<&Element<'a>> =
                    elements.iter().filter_map(|e| e.get_links()).flatten().collect();
                if !links.is_empty() {
                    Some(links)
                } else {
                    None
                }
            }
            Element::FootnoteReference(_, _) => None,
            Element::HardBreak(_) => None,
            Element::Html(_, _) => None,
            Element::Image(_, _, _, elements, _) => {
                let mut links: Vec<&Element<'_>> =
                    elements.iter().filter_map(|e| e.get_links()).flatten().collect();
                links.push(self);
                Some(links)
            }
            Element::Link(_, _, _, _, _) => Some(vec![self]),
            Element::List(_, elements, _) => {
                let links: Vec<&Element<'a>> =
                    elements.iter().filter_map(|e| e.get_links()).flatten().collect();
                if !links.is_empty() {
                    Some(links)
                } else {
                    None
                }
            }
            Element::Rule(_) => None,
            Element::SoftBreak(_) => None,
            Element::TaskListMarker(_, _) => None,
            Element::Text(_, _) => None,
        }
    }

    fn get_elements_mut(&mut self) -> Option<&mut Vec<Element<'a>>> {
        match self {
            Element::Block(_, ref mut elements, _) => Some(elements),
            Element::Code(_, _) => None,
            Element::CodeBlock(_, ref mut elements, _) => Some(elements),
            Element::FootnoteReference(_, _) => None,
            Element::HardBreak(_) => None,
            Element::Html(_, _) => None,
            Element::Image(_, _, _, ref mut elements, _) => Some(elements),
            Element::Link(_, _, _, ref mut elements, _) => Some(elements),
            Element::List(_, ref mut elements, _) => Some(elements),
            Element::Rule(_) => None,
            Element::SoftBreak(_) => None,
            Element::TaskListMarker(_, _) => None,
            Element::Text(_, _) => None,
        }
    }

    /**
     * Add the delta to the line number of this element and all nested elements.
     */
    fn add_line_offset(&mut self, delta: i32) {
        let mut doc_line = self.doc_line_mut();
        let n: i32 = doc_line.line_num.try_into().unwrap();
        doc_line.line_num = (n + delta).try_into().unwrap();

        if let Some(elements) = self.get_elements_mut() {
            elements.iter_mut().for_each(|f| f.add_line_offset(delta));
        }
    }

    /**
     * Public method to correct the line number of this element, the nested elements
     * are updated relatively.
     */
    pub fn set_line_num(&mut self, line_num: usize) {
        let doc_line = self.doc_line();
        let new: i32 = line_num.try_into().unwrap();
        let old: i32 = doc_line.line_num.try_into().unwrap();
        let delta: i32 = new - old;
        if delta != 0 {
            self.add_line_offset(delta);
        }
    }
}

pub struct DocContext<'a> {
    pub file_name: PathBuf,
    pub line_num: usize,
    pub(crate) parser: pulldown_cmark::OffsetIter<'a>,
    file_text: &'a str,
    line_index: HashMap<&'a str, usize>,
}

impl<'a> DocContext<'a> {
    fn link_callback(reference: &str, normalized: &str) -> Option<(String, String)> {
        // TODO(fxbug.dev/118482): Glossary reference links are hard to validate.
        if !reference.starts_with("glossary.")  &&  
       // TODO(fxbug.dev/118522): Consider removing [TOC]
       reference != "TOC" &&
       // TODO(fxbug.dev/117520): need to check for anchors and classes.
       !reference.starts_with("#")
        {
            Some((reference.to_string(), normalized.to_string()))
        } else {
            None
        }
    }

    #[cfg(test)]
    pub fn new(filename: PathBuf, text: &'a str) -> DocContext<'a> {
        Self::new_with_checks(filename, text, false)
    }

    pub fn new_with_checks(
        filename: PathBuf,
        text: &'a str,
        check_reference_links: bool,
    ) -> DocContext<'a> {
        let options = Options::empty();
        let cb: Option<&'a dyn Fn(&str, &str) -> Option<(String, String)>> =
            if check_reference_links { Some(&Self::link_callback) } else { None };
        let mut index = HashMap::new();
        let lines = text.lines();
        let mut line_num = 1;
        for l in lines {
            index.insert(l, line_num);
            line_num += 1;
        }
        DocContext {
            file_name: filename,
            line_num: 1,
            parser: Parser::new_with_broken_link_callback(text, options, cb).into_offset_iter(),
            file_text: text,
            line_index: index,
        }
    }

    /**
     * Returns the span of the file being parsed for the given range.
     */
    pub fn span(&self, range: &Range<usize>) -> &'a str {
        if range.end > range.start {
            &self.file_text[range.start..range.end]
        } else {
            &self.file_text[range.start..]
        }
    }

    /**
     * Returns the line number of the given span. The span must equal the entire line.
     * newline ending is stripped.
     */
    pub fn line_number_of(&self, span: &str) -> Option<usize> {
        let key = span.strip_suffix('\n').unwrap_or_else(|| span);
        self.line_index.get(key).copied()
    }

    pub fn line(&self) -> DocLine {
        DocLine { line_num: self.line_num, file_name: self.file_name.clone() }
    }
}

impl<'a> Iterator for DocContext<'a> {
    type Item = Element<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        self.parser.next().map(|(event, range)| parser::element_from_event(event, range, self))
    }
}

#[cfg(test)]
mod test {
    use super::Element::{Link, Text};
    use super::*;
    use anyhow::Result;
    use pulldown_cmark::CowStr::Borrowed;
    use pulldown_cmark::LinkType::Inline;

    #[test]
    fn test_get_links() -> Result<()> {
        let test_data: Vec<(DocContext<'static>, Option<Vec<Element<'static>>>)> = vec![
        (
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                "This has no links",
            ),
            None
        ),
        (
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                r#"codeblock has no links

```sh
This is an example [link](https://somewhere.com)
```
"#,
            ),
            None
        ),
        (
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                "This is a line to [something-one-line](/docs/something.md)",
            ),
            Some(vec![
                Link(Inline, Borrowed("/docs/something.md"), Borrowed(""),
                 vec![Text(Borrowed("something-one-line"), DocLine { line_num: 1, file_name: "/docs/README.md".into() })],
                 DocLine { line_num: 1, file_name: "/docs/README.md".into() })
            ])
        ),
        (
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                "This is a multiline\n\nparagraph. This is a line to [something-two-line](/docs/something.md)",
            ),
            Some(vec![
                Link(Inline, Borrowed("/docs/something.md"), Borrowed(""),
                 vec![Text(Borrowed("something-two-line"), DocLine { line_num: 3,file_name: "/docs/README.md".into() })],
                 DocLine { line_num: 3, file_name: "/docs/README.md".into() })
            ])
        ),
        (
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                r#"list item
* one item
* one with [something](/docs/in-list-item.md)

                "#
            ),
            Some(vec![
                Link(Inline, Borrowed("/docs/in-list-item.md"), Borrowed(""),
                vec![Text(Borrowed("something"), DocLine { line_num: 4, file_name: "/docs/README.md".into() })],
                DocLine { line_num: 4, file_name: "/docs/README.md".into() })
            ])
        ),
        (
            DocContext::new(
                PathBuf::from("/docs/README.md"),
                r#"list item
* one item

    In a paragraph one with [something](/docs/in-list-item-pp.md)
* item two
                "#
            ),
            Some(vec![
                Link(Inline, Borrowed("/docs/in-list-item-pp.md"), Borrowed(""),
                vec![Text(Borrowed("something"), DocLine { line_num: 5, file_name: "/docs/README.md".into() })],
                DocLine { line_num: 5, file_name: "/docs/README.md".into() })
            ])
        ),
        ];

        for (ctx, expected_links) in test_data {
            // Collect all the links from the markdown fragment.
            let elements = ctx.collect::<Vec<Element<'_>>>();
            let actual_links: Vec<&Element<'_>> =
                elements.iter().filter_map(|e| e.get_links()).flatten().collect();
            if !actual_links.is_empty() {
                if let Some(ref expected_list) = expected_links {
                    let mut expected_iter = expected_list.iter();
                    for actual in actual_links {
                        if let Some(expected) = expected_iter.next() {
                            assert_eq!(actual, expected);
                        } else {
                            panic!("Got unexpected link returned: {:?}", actual);
                        }
                    }
                    let unused_links: Vec<&Element<'_>> = expected_iter.collect();
                    if !unused_links.is_empty() {
                        panic!("Expected more links: {:?}", unused_links);
                    }
                } else {
                    panic!("Got unexpected links (expected is None): {:?}", actual_links);
                }
            } else if expected_links.is_some() {
                panic!("No links, but expected {:?}", expected_links);
            }
        }

        Ok(())
    }
}
