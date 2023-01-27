# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Rebase paths in product config to be relative to artifact_base_path."""

import argparse
import json
from typing import Dict, Any


def parse_args():
    """Parses command-line arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument(
        '--product-config-path',
        help='Path to product config',
        required=True,
    )
    parser.add_argument(
        '--additional-bool',
        help='Additional bool flags',
    )
    parser.add_argument(
        '--additional-string',
        help='Additional string flags',
    )
    parser.add_argument(
        '--additional-int',
        help='Additional int flags',
    )
    parser.add_argument(
        '--output',
        help='Path to output product config',
        required=True,
    )
    return parser.parse_args()


def add_new_parameter(config: Dict[str, Any], key: str, value: str,
                      value_type: str):
    if '.' not in key:
        if value_type == 'bool':
            config[key] = value == 'true'
        if value_type == 'str':
            config[key] = value
        if value_type == 'int':
            config[key] = int(value)
        return config

    path_seg = key.split('.', 1)
    sub_config = config[path_seg[0]] if path_seg[0] in config else {}
    config[path_seg[0]] = add_new_parameter(sub_config, path_seg[1], value, value_type)
    return config


def main():
    args = parse_args()
    with open(args.product_config_path, 'r') as f:
        product_config_json = json.load(f)

    platform_config = product_config_json['platform']

    dict_bool = json.loads(args.additional_bool)
    for key in dict_bool:
        add_new_parameter(platform_config, key, dict_bool[key], 'bool')
    dict_int = json.loads(args.additional_int)
    for key in dict_int:
        add_new_parameter(platform_config, key, dict_int[key], 'int')
    dict_string = json.loads(args.additional_string)
    for key in dict_string:
        add_new_parameter(platform_config, key, dict_string[key], 'str')

    with open(args.output, 'w') as f:
        json.dump(product_config_json, f, indent=2)


if __name__ == '__main__':
    main()
