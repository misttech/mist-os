#!/usr/bin/env fuchsia-vendored-python
# Copyright 2020 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import os
import re
import textwrap
from pathlib import Path
from dataclasses import dataclass, asdict
from string import Template

FUCHSIA_DIR = Path(os.environ["FUCHSIA_DIR"])

# Docs paths
INDEX_FILE = FUCHSIA_DIR / "docs/reference/fidl/language/errcat.md"
REDIRECT_FILE = FUCHSIA_DIR / "docs/error/_redirects.yaml"
ERRCAT_DIR = FUCHSIA_DIR / "docs/reference/fidl/language/error-catalog"
ERRCAT_TXT = FUCHSIA_DIR / "docs/reference/fidl/language/error-catalog/_files.txt"

# fidlc paths
BAD_FIDL_DIR = FUCHSIA_DIR / "tools/fidl/fidlc/tests/fidl/bad"
BAD_FIDL_TXT = FUCHSIA_DIR / "tools/fidl/fidlc/tests/fidl/bad/files.txt"
GOOD_FIDL_DIR = FUCHSIA_DIR / "tools/fidl/fidlc/tests/fidl/good"
GOOD_FIDL_TXT = FUCHSIA_DIR / "tools/fidl/fidlc/tests/fidl/good/files.txt"
GOOD_TESTS_CC = FUCHSIA_DIR / "tools/fidl/fidlc/tests/errcat_good_tests.cc"

# Templates
DOC_TEMPLATE = FUCHSIA_DIR / "tools/fidl/scripts/add_errcat_templates/doc.md.template"
INCLUDECODE_TEMPLATE = FUCHSIA_DIR / "tools/fidl/scripts/add_errcat_templates/includecode.md.template"
BAD_TEMPLATE = FUCHSIA_DIR / "tools/fidl/scripts/add_errcat_templates/bad.fidl.template"
GOOD_TEMPLATE = FUCHSIA_DIR / "tools/fidl/scripts/add_errcat_templates/good.fidl.template"

# Regexes used to find insertion locations.
REGEX_INDEX_ENTRY = r'^<<error-catalog/_fi-(\d+)\.md>>'
REGEX_REDIRECT_ENTRY = r'^- from: /fuchsia-src/error/fi-(\d+)'
REGEX_ERRCAT_TXT_LINE = r'^_fi-(\d+)\.md$'
REGEX_FIDL_TXT_LINE = r'^fi-(\d+)(?:-[a-z])?\.test.fidl$'
REGEX_GOOD_TEST = r'TEST\(ErrcatGoodTests, Good(\d+)'

# Use this weird string concatenation so that this string literal does not show
# up in search results when people grep for it.
DNS = "DO" + "NOT" + "SUBMIT"

# When true, this script just prints what would happen if run for real.
dry_run = False


def find_error_num(regex, line):
    result = re.search(regex, line)
    if result is None:
        return None
    return int(result.groups(1)[0])


def insert_lines_at_line_num(path, lines, line_num, line):
    lines.insert(line_num, line + "\n")
    if not dry_run:
        with open(path, "w") as f:
            f.write("".join(lines))


# Scans each line in path and tries mathcing with entry_matcher, whose $1 group
# should be 4 digits e.g. 1234 in fi-1234. Once it finds a line where than
# number exceeds num, inserts to_insert just before it. If none exceed num,
# starts at the last matching line, searches for after_regex (if provided), goes
# further down by after_offset lines, and inserts to_insert there.
def insert_entry(
        path, entry_matcher, num, to_insert, after_offset, after_regex=None):
    # Only add an entry for this numeral if none already exists.
    with open(path) as f:
        lines = f.readlines()

    # Do a pass over the lines in the file to ensure that this entry does not already exist.
    if any(line.startswith(to_insert.split('\n')[0]) for line in lines):
        return 0

    # If we do need a file do another pass, inserting the supplied text in the correct position.
    insert_after = None
    for line_num, line in enumerate(lines):
        entry_num = find_error_num(entry_matcher, line)
        if entry_num is None:
            continue
        if entry_num > num:
            insert_lines_at_line_num(path, lines, line_num, to_insert)
            return 1
        insert_after = line_num

    # Handle the edge case of the entry needing to be placed at the end of the list.
    if insert_after is None:
        raise Exception(f"never found regex {entry_matcher} in {path}")
    if after_regex:
        while insert_after < len(lines) and not re.search(after_regex,
                                                          lines[insert_after]):
            insert_after += 1
    insert_lines_at_line_num(
        path, lines, insert_after + after_offset, to_insert)
    return 1


def substitute(template_path, subs):
    subs['dns'] = DNS
    with open(template_path) as f:
        template = Template(f.read())
    return template.substitute(subs)


def create_file(file_path, template_path, subs):
    if file_path.is_file():
        return 0
    if not dry_run:
        with open(file_path, "w") as f:
            f.write(substitute(template_path, subs))
    return 1


@dataclass
class Change:
    markdown_file_created: int = 0
    markdown_txt_line_added: int = 0
    index_entry_added: int = 0
    redirect_entry_added: int = 0
    bad_fidl_created: int = 0
    good_fidl_created: int = 0
    bad_txt_lines_added: int = 0
    good_txt_lines_added: int = 0
    good_tests_added: int = 0


def main(args):
    global dry_run
    dry_run = args.dry_run

    change = Change()
    n = args.numeral
    ns = f"{n:04}"

    # Add an index entry if none exists.
    change.index_entry_added = insert_entry(
        INDEX_FILE,
        REGEX_INDEX_ENTRY,
        n,
        f"<<error-catalog/_fi-{ns}.md>>\n",
        after_offset=2)

    # Add a redirect entry if none exists.
    change.redirect_entry_added = insert_entry(
        REDIRECT_FILE,
        REGEX_REDIRECT_ENTRY,
        n,
        f'- from: /fuchsia-src/error/fi-{ns}\n  to: /fuchsia-src/reference/fidl/language/errcat.md#fi-{ns}',
        after_offset=2)

    # Create .test.fidl files for "bad" examples, and add to BUILD.gn.
    bad_includecodes = []
    for b in range(args.bad):
        suffix = chr(ord('a') + b) if args.bad > 1 else ""
        case_name = "fi-" + ns + ("-" + suffix if suffix else "")
        flat_name = ns + suffix

        # Add any new files we need.
        filename = f"{case_name}.test.fidl"
        change.bad_fidl_created += create_file(
            BAD_FIDL_DIR / filename, BAD_TEMPLATE, {
                'num': ns,
                'case_name': case_name,
                'flat_name': flat_name,
            })

        # Insert this filename into files.txt.
        change.bad_txt_lines_added += insert_entry(
            BAD_FIDL_TXT, REGEX_FIDL_TXT_LINE, n, filename, after_offset=1)

        # Create the markdown entry for this case.
        bad_includecodes.append(
            substitute(
                INCLUDECODE_TEMPLATE, {
                    'kind': "bad",
                    'case_name': case_name,
                }))

    # Create .test.fidl files for "good" examples, and add to BUILD.gn.
    good_includecodes = []
    for g in range(args.good):
        suffix = chr(ord('a') + g) if args.good > 1 else ""
        case_name = "fi-" + ns + ("-" + suffix if suffix else "")
        flat_name = ns + suffix

        # Add any new files we need.
        filename = f"{case_name}.test.fidl"
        change.good_fidl_created += create_file(
            GOOD_FIDL_DIR / filename, GOOD_TEMPLATE, {
                'num': ns,
                'case_name': case_name,
                'flat_name': flat_name,
            })

        # Insert this filename into files.txt.
        change.good_txt_lines_added += insert_entry(
            GOOD_FIDL_TXT, REGEX_FIDL_TXT_LINE, n, filename, after_offset=1)

        # Create the markdown entry for this case.
        good_includecodes.append(
            substitute(
                INCLUDECODE_TEMPLATE, {
                    'kind': "good",
                    'case_name': case_name,
                }))

        # Add a test case if none exists.
        insert = textwrap.dedent(
            f"""\
            TEST(ErrcatTests, Good{flat_name}) {{
              TestLibrary library;
              library.AddFile("good/{filename}");
              ASSERT_COMPILED(library);
            }}
            """)
        change.good_tests_added += insert_entry(
            GOOD_TESTS_CC,
            REGEX_GOOD_TEST,
            n,
            insert,
            after_offset=2,
            after_regex=r"^}$")

    # Create the Markdown file for the actual doc.
    markdown_filename = f"_fi-{ns}.md"
    change.markdown_file_created = create_file(
        ERRCAT_DIR / markdown_filename, DOC_TEMPLATE, {
            'num': ns,
            'good_includecodes': '\n'.join(good_includecodes),
            'bad_includecodes': '\n'.join(bad_includecodes),
        })

    # Insert the Markdown filename into _files.txt.
    change.markdown_txt_line_added += insert_entry(
        ERRCAT_TXT, REGEX_ERRCAT_TXT_LINE, n, markdown_filename, after_offset=1)

    if not (any(asdict(change).values())):
        print(f"There is nothing to do for fi-{ns}")
        return

    added = "Would add" if dry_run else "Added"
    created = "Would create" if dry_run else "Created"
    rel = lambda path: path.relative_to(Path.cwd())
    if change.markdown_file_created:
        print(f"* {created} {rel(ERRCAT_DIR / markdown_filename)}")
    if change.markdown_txt_line_added:
        print(f"* {added} 1 line to {rel(ERRCAT_TXT)}")
    if change.index_entry_added:
        print(f"* {added} index entry in {rel(INDEX_FILE)}")
    if change.redirect_entry_added:
        print(f"* {added} redirect entry in {rel(REDIRECT_FILE)}")
    if change.bad_fidl_created:
        print(
            f"* {created} {change.bad_fidl_created} file(s) in {rel(BAD_FIDL_DIR)}"
        )
    if change.good_fidl_created:
        print(
            f"* {created} {change.good_fidl_created} file(s) in {rel(GOOD_FIDL_DIR)}"
        )
    if change.bad_txt_lines_added:
        print(
            f"* {added} {change.bad_txt_lines_added} line(s) to {rel(BAD_FIDL_TXT)}"
        )
    if change.good_txt_lines_added:
        print(
            f"* {added} {change.good_txt_lines_added} line(s) to {rel(GOOD_FIDL_TXT)}"
        )
    if change.good_tests_added:
        print(
            f"* {added} {change.good_tests_added} test case(s) to {rel(GOOD_TESTS_CC)}"
        )


if __name__ == '__main__':
    parser = argparse.ArgumentParser(
        description=textwrap.dedent(
            """
            Adds an entry to //docs/references/fidl/language/errcat.md,
            generating or updating all files as necessary to document the error.
            It is safe to run multiple times. It only adds things that are missing.
            """),
        formatter_class=argparse.RawTextHelpFormatter)
    parser.add_argument(
        "numeral",
        metavar='NUMBER',
        type=int,
        help='The error ID number as defined in diagnostics.h')
    parser.add_argument(
        "-b",
        "--bad",
        type=int,
        default=1,
        help='Number of "bad" example .test.fidl files to generate')
    parser.add_argument(
        "-g",
        "--good",
        type=int,
        default=1,
        help='Number of "good" example .test.fidl files to generate')
    parser.add_argument(
        "-n",
        "--dry-run",
        action="store_true",
        help='Just print what would happen, do not create or edit files')
    main(parser.parse_args())
