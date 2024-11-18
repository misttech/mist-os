# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import functools
import json
import os
import typing

import environment


class PackageRepositoryError(Exception):
    """Raised when there was an issue processing the package repository."""


class PackageRepository:
    """Wrapper for package repository data created during the build process."""

    def __init__(self, name_to_merkle: dict[str, str]):
        """Create a representation of a package directory.

        Args:
            name_to_merkle (dict[str, str]): Mapping of package name to merkle hash.
        """
        self.name_to_merkle = name_to_merkle

    @classmethod
    @functools.lru_cache()
    def from_env(
        cls, exec_env: environment.ExecutionEnvironment
    ) -> typing.Self:
        """Create a package repository wrapper from an environment.

        Args:
            exec_env (environment.ExecutionEnvironment): Environment to load from.

        Raises:
            PackageRepositoryError: If there was an issue loading the package repository information.
        """
        targets_path: str

        if exec_env.package_repositories_file is None:
            raise PackageRepositoryError(
                "No package-repositories.json file was found for the build."
            )

        try:
            with open(exec_env.package_repositories_file, "r") as f:
                targets_path = os.path.join(
                    os.path.dirname(exec_env.package_repositories_file),
                    json.load(f)[0]["targets"],
                )

            name_to_merkle: dict[str, str] = dict()
            with open(targets_path, "r") as f:
                entries = json.load(f)
                key: str
                value: dict[str, typing.Any]
                for key, value in entries["signed"]["targets"].items():
                    if "custom" in value:
                        name, _ = key.split("/")
                        name_to_merkle[name] = value["custom"]["merkle"]

            return cls(name_to_merkle)

        except Exception as e:
            raise PackageRepositoryError(
                f"Error loading package repository: {e}"
            )
