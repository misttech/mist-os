# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Rebase paths in product config to be relative to artifact_base_path."""

import argparse
import json
import os


def parse_args():
    """Parses command-line arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument(
        '--product-config',
        type=argparse.FileType('r'),
        help='original product config file',
        required=True,
    )
    parser.add_argument(
        '--updated-product-config',
        type=argparse.FileType('w'),
        help='output product config file',
        required=True,
    )
    parser.add_argument(
        '--base-details',
        help='Details of additional base packages',
        required=True,
    )
    parser.add_argument(
        '--relative-base',
        help='Path to artifact base',
        required=True,
    )
    return parser.parse_args()


def main():
    args = parse_args()
    base_path = args.relative_base
    product_config_json = json.load(args.product_config)
    additional_base_pkgs = json.loads(args.base_details)
    if not additional_base_pkgs:
        json.dump(product_config_json, args.updated_product_config, indent=2)
        return

    for package in additional_base_pkgs:
        package['manifest'] = os.path.relpath(package['manifest'], base_path)
        if 'config_data' not in package:
            continue
        for config_data in package['config_data']:
            config_data['source'] = os.path.relpath(config_data['source'],
                                                    base_path)
    if 'base' not in product_config_json['product']['packages']:
        product_config_json['product']['packages']['base'] = []
    product_config_json['product']['packages']['base'].extend(
        additional_base_pkgs)

    json.dump(product_config_json, args.updated_product_config, indent=2)


if __name__ == '__main__':
    main()
