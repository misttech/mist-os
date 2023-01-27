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
        '--product-config-content',
        help='Content of product config',
        required=True,
    )
    parser.add_argument(
        '--product-config-path',
        help='Path to output product config',
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
    product_config_json = json.loads(args.product_config_content)
    product_config = product_config_json['product']
    for driver in product_config['drivers']:
        driver['package'] = os.path.relpath(driver['package'], base_path)
    packages = product_config['packages']['base']
    cache_packages = product_config['packages']['cache']
    for package in packages + cache_packages:
        package['manifest'] = os.path.relpath(package['manifest'], base_path)
        if 'config_data' not in package:
            continue
        for config_data in package['config_data']:
            config_data['source'] = os.path.relpath(config_data['source'],
                                                    base_path)

    with open(args.product_config_path, 'w') as f:
        json.dump(product_config_json, f, indent=2)


if __name__ == '__main__':
    main()
