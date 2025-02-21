#!/usr/bin/env python3

# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import re
import sys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--spdx_id", default="")
    parser.add_argument("--license_id", required=True)
    parser.add_argument("--cut_after", default=False)
    parser.add_argument("--cut_entirely", action=argparse.BooleanOptionalAction)
    parser.add_argument("--exit_on_failure", default=False)
    args = parser.parse_args()

    with open(args.input, "r") as spdx:
        data = json.load(spdx)

    found_segment = False
    error_msg = "Did not find licenseID {} in spdx file {}.".format(
        args.license_id, args.input
    )
    if args.cut_entirely:
        # Delete the license section from the SPDX file.
        i = 0
        while i < len(data["hasExtractedLicensingInfos"]):
            d = data["hasExtractedLicensingInfos"][i]
            if d["licenseId"] == args.license_id:
                data["hasExtractedLicensingInfos"].pop(i)
                break
            i += 1

        # Delete the "licenseConcluded" field in the packages section.
        i = 0
        while i < len(data["packages"]):
            p = data["packages"][i]
            if (
                "licenseConcluded" in p
                and p["licenseConcluded"] == args.license_id
            ):
                data["packages"][i].pop("licenseConcluded")
            i += 1
    else:
        for d in data["hasExtractedLicensingInfos"]:
            if d["licenseId"] == args.license_id:
                error_msg = (
                    "Did not find string pattern {} in license text.".format(
                        args.cut_after
                    )
                )
                text = d["extractedText"]
                match = re.search(args.cut_after, text)
                if match:
                    d["extractedText"] = text[: match.end()]
                    found_segment = True
                break

    if not found_segment:
        if args.exit_on_failure:
            raise ValueError(error_msg)
        else:
            print(error_msg)

    with open(args.output, "w") as spdx:
        json.dump(data, spdx, ensure_ascii=False, indent="    ")


if __name__ == "__main__":
    sys.exit(main())
