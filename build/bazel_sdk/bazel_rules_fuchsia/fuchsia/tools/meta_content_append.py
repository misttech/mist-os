# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Create a meta/content file."""

import argparse
import json
import os
import subprocess


def run(*command):
    return subprocess.check_output(
        command,
        text=True,
    ).strip()


def parse_args():
    """Parses command-line arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--ffx",
        help="Path to ffx_package",
        required=True,
    )
    parser.add_argument(
        "--manifest-path",
        help="Path to output manifest file",
        required=True,
    )
    parser.add_argument(
        "--meta-contents-dir",
        help="Path to output meta contents dir",
        required=True,
    )
    parser.add_argument(
        "--ffx-isolate-dir",
        help="Path to ffx isolate dir",
        required=True,
    )
    parser.add_argument(
        "--original-content",
        help="Original contents before appending.",
        required=True,
    )
    parser.add_argument(
        "--subpackage-manifests",
        help="Additional subpackage manifests need to be flatten",
        required=True,
        nargs="+",
    )
    return parser.parse_args()


def write_file(manifest_path, content_map):
    # Sort by dest for deterministic ordering.
    meta_contents = [
        dest + "=" + src for dest, src in sorted(content_map.items())
    ]
    with open(manifest_path, "w") as f:
        f.writelines("\n".join(meta_contents))


def collect_component_manifests(
    ffx_tool, ffx_isolate_dir, blob_source_path, extract_dir
):
    content_map = {}
    run(
        ffx_tool,
        "--isolate-dir",
        ffx_isolate_dir,
        "package",
        "far",
        "extract",
        blob_source_path,
        "-o",
        extract_dir,
    )
    for currentpath, folders, files in os.walk(extract_dir):
        for file in files:
            fullpath = os.path.join(currentpath, file)
            dest = os.path.relpath(fullpath, extract_dir)
            if dest in ["meta/contents", "meta/package"]:
                continue
            content_map[dest] = fullpath
    return content_map


def main():
    args = parse_args()
    content_map = {}
    contents = args.original_content.split("\n")

    for content in contents:
        src, dest = content.split("=")
        content_map[src] = dest

    for manifest in args.subpackage_manifests:
        manifest_dir = os.path.dirname(manifest)
        with open(manifest, "r") as f:
            manifest_json = json.load(f)
            for blob in manifest_json["blobs"]:
                blob_source_path = os.path.join(
                    manifest_dir, blob["source_path"]
                )
                if blob["path"] in content_map:
                    continue
                if blob["path"] == "meta/":
                    content_map.update(
                        collect_component_manifests(
                            args.ffx,
                            args.ffx_isolate_dir,
                            blob_source_path,
                            args.meta_contents_dir,
                        )
                    )
                else:
                    content_map[blob["path"]] = blob_source_path

    # Remove ABI stamp file. It gets specified on the command line of `ffx
    # package build`, and therefore can't appear in manifests.
    del content_map["meta/fuchsia.abi/abi-revision"]

    write_file(args.manifest_path, content_map)


if __name__ == "__main__":
    main()
