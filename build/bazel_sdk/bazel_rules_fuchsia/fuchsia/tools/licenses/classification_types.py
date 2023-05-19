# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Types for classifying licenses"""

from collections import defaultdict
import csv
import dataclasses
import json
from fuchsia.tools.licenses.common_types import *
from fuchsia.tools.licenses.spdx_types import *
from hashlib import md5
from typing import Any, Callable, ClassVar, Dict, Pattern, List


@dataclasses.dataclass(frozen=True)
class IdentifiedSnippet:
    """Information about a single license snippet (text part of a large license text)"""

    # 'identified_as' value for unidentified snippets.
    UNIDENTIFIED_IDENTIFICATION: ClassVar[str] = "[UNIDENTIFIED]"

    identified_as: str
    confidence: float
    start_line: int
    end_line: int

    condition: str = None
    # Conditions from overriding rules
    overriden_conditions: List[str] = None
    # Optional public source code mirroring urls (supplied by some override rules)
    public_source_mirrors: List[str] = None
    # Whether the project is shipped.
    is_project_shipped: bool = None
    # Whether notice is shipped.
    is_notice_shipped: bool = None
    # Whether source code is shipped.
    is_source_code_shipped: bool = None
    # Dependents that were not matched by any rule
    dependents_unmatched_by_overriding_rules: List[str] = None
    # all rules that matched this IdentifiedSnippet
    overriding_rules: List["ConditionOverrideRule"] = None

    # verification results
    verified: bool = None
    verification_message: str = None

    # checksum for snippet text
    snippet_checksum: str = None
    snippet_text: str = None

    # A suggested override rule
    suggested_override_rule: "ConditionOverrideRule" = None

    def create_empty(extracted_text_lines, condition) -> "IdentifiedSnippet":
        return IdentifiedSnippet(
            identified_as=IdentifiedSnippet.UNIDENTIFIED_IDENTIFICATION,
            confidence=1.0,
            start_line=1,
            end_line=len(extracted_text_lines) + 1,
            condition=condition)

    def from_identify_license_dict(
            dictionary: Dict[str, Any], location: Any,
            default_condition: str) -> "IdentifiedSnippet":
        """
        Create a IdentifiedSnippet instance from a dictionary in the output format of
        https://github.com/google/licenseclassifier/tree/main/tools/identify_license.

        i.e.
        {
            "Name": str
            "Confidence": int or float
            "StartLine": int
            "EndLine": int
            "Condition": str
        }

        "Name" will be "Unclassified" for licenses that the tool can't identify.
        """
        r = DictReader(dictionary, location)

        identified_as = r.get('Name')
        if identified_as == 'Unclassified':
            identified_as = IdentifiedSnippet.UNIDENTIFIED_IDENTIFICATION

        # Confidence could be an int or a float. Convert to a float.
        try:
            confidence = r.get('Confidence', expected_type=float)
        except LicenseException:
            confidence = float(r.get('Confidence', expected_type=int))

        condition = r.get_or('Condition', expected_type=str, default=None)
        if not condition:
            condition = default_condition

        return IdentifiedSnippet(
            identified_as=identified_as,
            confidence=confidence,
            start_line=r.get('StartLine', expected_type=int),
            end_line=r.get('EndLine', expected_type=int),
            condition=condition)

    def to_json_dict(self):
        # The fields are output in a certain order to produce a more readable output.
        out = {
            "identified_as": self.identified_as,
            "condition": self.condition,
            "verified": self.verified,
        }

        if self.verification_message:
            out["verification_message"] = self.verification_message
        if self.overriden_conditions:
            out["overriden_conditions"] = self.overriden_conditions
        if self.dependents_unmatched_by_overriding_rules:
            out["dependents_unmatched_by_overriding_rules"] = self.dependents_unmatched_by_overriding_rules
        if self.overriding_rules:
            out["overriding_rules"] = [
                r.to_json_dict() for r in self.overriding_rules
            ]
        if self.suggested_override_rule:
            out["suggested_override_rule"] = self.suggested_override_rule.to_json_dict(
            )
        if self.public_source_mirrors:
            out["public_source_mirrors"] = self.public_source_mirrors
        if self.is_project_shipped != None:
            out["is_project_shipped"] = self.is_project_shipped
        if self.is_notice_shipped != None:
            out["is_notice_shipped"] = self.is_notice_shipped
        if self.is_source_code_shipped != None:
            out["is_source_code_shipped"] = self.is_source_code_shipped

        out.update(
            {
                "confidence": self.confidence,
                "start_line": self.start_line,
                "end_line": self.end_line,
                "snippet_checksum": self.snippet_checksum,
                "snippet_text": self.snippet_text,
            })
        return out

    def from_json_dict(reader: DictReader) -> "IdentifiedSnippet":
        suggested_override_rule = None
        if reader.exists("suggested_override_rule"):
            suggested_override_rule = ConditionOverrideRule.from_json_dict(
                reader.get_reader("suggested_override_rule"), reader.location)

        overriding_rules = None
        if reader.exists("overriding_rules"):
            overriding_rules = [
                ConditionOverrideRule.from_json_dict(r, reader.location)
                for r in reader.get_readers_list("overriding_rules")
            ]

        return IdentifiedSnippet(
            identified_as=reader.get("identified_as"),
            condition=reader.get("condition"),
            verified=reader.get_or("verified", default=False),
            verification_message=reader.get_or(
                "verification_message", default=None),
            overriden_conditions=reader.get_or(
                "overriden_conditions", default=None, expected_type=list),
            public_source_mirrors=reader.get_or(
                "public_source_mirrors", default=None, expected_type=list),
            is_project_shipped=reader.get_or(
                "is_project_shipped", default=None, expected_type=bool),
            is_notice_shipped=reader.get_or(
                "is_notice_shipped", default=None, expected_type=bool),
            is_source_code_shipped=reader.get_or(
                "is_source_code_shipped", default=None, expected_type=bool),
            dependents_unmatched_by_overriding_rules=reader.get_or(
                "dependents_unmatched_by_overriding_rules",
                default=None,
                expected_type=list),
            overriding_rules=overriding_rules,
            suggested_override_rule=suggested_override_rule,
            confidence=reader.get("confidence", expected_type=float),
            start_line=reader.get("start_line", expected_type=int),
            end_line=reader.get("end_line", expected_type=int),
            snippet_checksum=reader.get("snippet_checksum"),
            snippet_text=reader.get("snippet_text"),
        )

    def number_of_lines(self):
        return self.end_line - self.start_line + 1

    def add_snippet_text(self, lines: List[str]):
        text = '\n'.join(lines[self.start_line - 1:self.end_line])
        checksum = md5(text.encode('utf-8')).hexdigest()
        return dataclasses.replace(
            self, snippet_text=text, snippet_checksum=checksum)

    def set_is_shipped_defaults(
            self, default_is_project_shipped, default_is_notice_shipped,
            default_is_source_code_shipped) -> "IdentifiedSnippet":

        def default_if_none(value, default):
            if value == None:
                return default
            else:
                return value

        return dataclasses.replace(
            self,
            is_project_shipped=default_if_none(
                self.is_project_shipped, default_is_project_shipped),
            is_notice_shipped=default_if_none(
                self.is_notice_shipped, default_is_notice_shipped),
            is_source_code_shipped=default_if_none(
                self.is_source_code_shipped, default_is_source_code_shipped),
        )

    def is_identified(self):
        return self.identified_as != IdentifiedSnippet.UNIDENTIFIED_IDENTIFICATION

    def override_conditions(
            self, license: "LicenseClassification",
            rules: List["ConditionOverrideRule"]):
        all_matching_rules = []

        new_conditions = set()
        public_source_mirrors = set()

        remaining_dependents = set(license.dependents)
        for rule in rules:
            # Check that the in optimization in LicenseClassification was applied
            assert rule.match_license_names.matches(license.name)

            # Match identification, checksome, condition
            if not rule.match_identifications.matches(self.identified_as):
                continue
            if not rule.match_snippet_checksums.matches(self.snippet_checksum):
                continue
            if not rule.match_conditions.matches(self.condition):
                continue

            # Match dependents
            some_matching_dependents = rule.match_dependents.get_matches(
                license.dependents)
            if not some_matching_dependents:
                continue

            new_conditions.add(rule.override_condition_to)
            if rule.public_source_mirrors:
                public_source_mirrors.update(rule.public_source_mirrors)

            all_matching_rules.append(rule)
            for d in some_matching_dependents:
                if d in remaining_dependents:
                    remaining_dependents.remove(d)

        if all_matching_rules:
            return dataclasses.replace(
                self,
                overriden_conditions=sorted(list(new_conditions)),
                dependents_unmatched_by_overriding_rules=sorted(
                    list(remaining_dependents)),
                overriding_rules=all_matching_rules,
                public_source_mirrors=sorted(list(public_source_mirrors)),
            )
        else:
            return self

    def verify_conditions(
            self, license: "LicenseClassification",
            allowed_conditions: Set[str]):
        """Sets the 'verified' and 'verification_message' fields"""
        verified = True
        message = None
        diallowed_override_conditions = []
        if self.overriden_conditions:
            diallowed_override_conditions = [
                c for c in self.overriden_conditions
                if c not in allowed_conditions
            ]
        if not self.overriding_rules:
            # Simple case: No overriding rules were involved.
            if self.condition not in allowed_conditions:
                verified = False
                message = f"'{self.condition}' condition is not an allowed"
        elif diallowed_override_conditions:
            # Some overriding rules were involved: Check their overriding conditions.
            rule_paths = [
                r.rule_file_path
                for r in self.overriding_rules
                if r.override_condition_to in diallowed_override_conditions
            ]
            verified = False
            message = f"The conditions {diallowed_override_conditions} are not allowed."\
                        f" They were introduced by these rules: {rule_paths}."
        elif self.dependents_unmatched_by_overriding_rules:
            # Some license dependents didn't match any rule. Check the original
            # conditions.
            rule_paths = [r.rule_file_path for r in self.overriding_rules]
            if self.condition not in allowed_conditions:
                verified = False
                message = f"The overriding rules {rule_paths} changed the conditions to " \
                    f"{self.overriden_conditions} but the rules don't match the dependencies " \
                    f"{self.dependents_unmatched_by_overriding_rules} that remain with the " \
                    f"condition '{self.condition} that is not allowed'."

        if verified:
            assert message == None
            suggested_override_rule = None
        else:
            assert message != None
            suggested_override_rule = ConditionOverrideRule.suggested_for_snippet(
                license, self, allowed_conditions)

        return dataclasses.replace(
            self,
            verified=verified,
            verification_message=message,
            suggested_override_rule=suggested_override_rule)

    def detailed_verification_message(
            self, license: "LicenseClassification") -> str:
        """Returns a very detailed verification failure message or None"""

        if self.verified:
            return None

        dependents_str = "\n".join([f"  {d}" for d in license.dependents])
        license_links = "\n".join([f"  {l}" for l in license.links])
        snippet = self.snippet_text
        max_snippet_length = 1000
        if len(snippet) > max_snippet_length:
            snippet = snippet[0:max_snippet_length] + "<TRUNCATED>"

        message = f"""
License '{license.name}' has a snippet identified as '{self.identified_as}' [{self.condition}].

Verification message:
{self.verification_message}

License links:
{license_links}

The license is depended on by:
{dependents_str}

Snippet begin line: {self.start_line}
Snippet end line: {self.end_line}
Snippet checksum: {self.snippet_checksum}
Snippet: <begin>
{snippet}
<end>

To fix this verification problem you should either:
1. Remove the dependency on projects with this license in the dependent code bases.
2. If the dependency is required and approved by the legal council of your project,
   you apply a local condition override, such as:
{json.dumps(self.suggested_override_rule.to_json_dict(), indent=4)}
"""
        return message


@dataclasses.dataclass(frozen=True)
class LicenseClassification:
    """Classification results for a single license"""

    license_id: str
    identifications: List[IdentifiedSnippet]
    name: str = None
    links: List[str] = None
    dependents: List[str] = None

    # license size & identification stats
    size_bytes: int = None
    size_lines: int = None
    unidentified_lines: int = None

    def to_json_dict(self):
        out = {
            "license_id": self.license_id,
            "name": self.name,
            "links": self.links,
            "dependents": self.dependents,
            "identifications": [m.to_json_dict() for m in self.identifications],
            "identification_stats":
                {
                    "size_bytes": self.size_bytes,
                    "size_lines": self.size_lines,
                    "unidentified_lines": self.unidentified_lines,
                },
        }

        return out

    def from_json_dict(reader: DictReader) -> "LicenseClassification":
        identifications = [
            IdentifiedSnippet.from_json_dict(r)
            for r in reader.get_readers_list("identifications")
        ]
        stats_reader = reader.get_reader("identification_stats")

        return LicenseClassification(
            license_id=reader.get("license_id"),
            name=reader.get("name"),
            links=reader.get_string_list("links"),
            dependents=reader.get_string_list("dependents"),
            identifications=identifications,
            size_bytes=stats_reader.get_or(
                "size_bytes", default=None, expected_type=int,
                accept_none=True),
            size_lines=stats_reader.get_or(
                "size_lines", default=None, expected_type=int,
                accept_none=True),
            unidentified_lines=stats_reader.get_or(
                "unidentified_lines",
                default=None,
                expected_type=int,
                accept_none=True),
        )

    def add_license_information(self, index: SpdxIndex):
        spdx_license = index.get_license_by_id(self.license_id)
        snippet_lines = spdx_license.extracted_text_lines()
        identifications = [
            i.add_snippet_text(snippet_lines) for i in self.identifications
        ]
        links = []
        if spdx_license.cross_refs:
            links.extend(spdx_license.cross_refs)
        if spdx_license.see_also:
            links.extend(spdx_license.see_also)
        chains = index.dependency_chains_for_license(spdx_license)
        dependents = [">".join([p.name for p in chain]) for chain in chains]
        # Sort and dedup dependent chains: There might be duplicate chains since
        # the package names are not globally unique.
        dependents = sorted(set(dependents))
        return dataclasses.replace(
            self,
            identifications=identifications,
            name=spdx_license.name,
            links=links,
            dependents=dependents,
        )

    def compute_identification_stats(self, index: SpdxIndex):
        spdx_license = index.get_license_by_id(self.license_id)

        extracted_text = spdx_license.extracted_text
        extracted_lines = spdx_license.extracted_text_lines()

        lines_identified = 0
        for identification in self.identifications:
            lines_identified += identification.number_of_lines()

        return dataclasses.replace(
            self,
            size_bytes=len(extracted_text),
            size_lines=len(extracted_lines),
            unidentified_lines=len(extracted_lines) - lines_identified,
        )

    def _transform_identifications(
        self, function: Callable[[IdentifiedSnippet], IdentifiedSnippet]
    ) -> "LicenseClassification":
        """Returns a copy of this object with the identifications transformed by function"""
        return dataclasses.replace(
            self, identifications=[function(i) for i in self.identifications])

    def override_conditions(self, rule_set: "ConditionOverrideRuleSet"):
        # Optimize by filtering rules that match the license name and any dependents
        relevant_rules = []
        for rule in rule_set.rules:
            if rule.match_license_names.matches(self.name):
                if rule.match_dependents.matches_any(self.dependents):
                    relevant_rules.append(rule)

        if relevant_rules:
            return self._transform_identifications(
                lambda x: x.override_conditions(self, relevant_rules))
        else:
            return self

    def verify_conditions(self, allowed_conditions: Set[str]):
        return self._transform_identifications(
            lambda x: x.verify_conditions(self, allowed_conditions))

    def verification_errors(self) -> List[str]:
        out = []
        for i in self.identifications:
            msg = i.detailed_verification_message(self)
            if msg:
                out.append(msg)
        return out

    def all_public_source_mirrors(self) -> List[str]:
        out = []
        for i in self.identifications:
            if i.public_source_mirrors:
                out.extend(i.public_source_mirrors)
        return sorted(list(set(out)))

    def is_project_shipped(self) -> bool:
        for i in self.identifications:
            if i.is_project_shipped:
                return True
        return False

    def is_notice_shipped(self) -> bool:
        for i in self.identifications:
            if i.is_notice_shipped:
                return True
        return False

    def is_source_code_shipped(self) -> bool:
        for i in self.identifications:
            if i.is_source_code_shipped:
                return True
        return False


@dataclasses.dataclass(frozen=True)
class LicensesClassifications:
    classifications_by_id: Dict[str, LicenseClassification]

    def create_empty() -> "LicenseClassification":
        return LicensesClassifications(classifications_by_id={})

    def from_identify_license_output_json(
            identify_license_output_path: str,
            license_paths_by_license_id: Dict[str, str],
            default_condition: str) -> "LicensesClassifications":
        json_output = json.load(open(identify_license_output_path, 'r'))

        # Expected results from https://github.com/google/licenseclassifier/tree/main/tools/identify_license
        # have the following json layout:
        # [
        #     {
        #         "Filepath": ...
        #         "Classifications: [
        #             {
        #                 "Name": ...
        #                 "Confidence": int or float
        #                 "StartLine": int
        #                 "EndLine": int
        #                 "Condition": str
        #             },
        #             { ...},
        #             ...
        #         ]
        #     },
        #     { ... },
        #     ...
        # ]

        results_by_file_path = {}
        for one_output in json_output:
            file_name = one_output['Filepath']
            assert file_name not in results_by_file_path
            results_by_file_path[file_name] = one_output['Classifications']

        identifications_by_license_id = defaultdict(list)
        for license_id, file_name in license_paths_by_license_id.items():
            if file_name in results_by_file_path.keys():
                for match_json in results_by_file_path[file_name]:
                    identifications_by_license_id[license_id].append(
                        IdentifiedSnippet.from_identify_license_dict(
                            dictionary=match_json,
                            location=identify_license_output_path,
                            default_condition=default_condition))
        license_classifications = {}
        for license_id, identifications in identifications_by_license_id.items(
        ):
            license_classifications[license_id] = LicenseClassification(
                license_id=license_id, identifications=identifications)

        return LicensesClassifications(license_classifications)

    def to_json_list(self) -> List[Any]:
        output = []
        for license_id in sorted(self.classifications_by_id.keys()):
            output.append(self.classifications_by_id[license_id].to_json_dict())
        return output

    def to_json(self, json_file_path: str):
        with open(json_file_path, 'w') as output_file:
            json.dump(self.to_json_list(), output_file, indent=4)

    def from_json_list(
            input: List[Any], location: str) -> "LicensesClassifications":
        if not isinstance(input, List):
            raise LicenseException(
                f"Expected a list of classification json values, but got {type(input)}",
                location)
        classifications_by_id = {}
        for value in input:
            if not isinstance(value, dict):
                raise LicenseException(
                    f"Expected json dict but got {type(input)}", location)
            value_reader = DictReader(value, location)
            classification = LicenseClassification.from_json_dict(value_reader)
            if classification.license_id in classifications_by_id:
                raise LicenseException(
                    f"Multiple classifications with license_id '{classification.license_id}'",
                    location)
            classifications_by_id[classification.license_id] = classification

        return LicensesClassifications(classifications_by_id)

    def from_json(json_file_path: str) -> "LicensesClassifications":
        with open(json_file_path, "r") as f:
            try:
                json_obj = json.load(f)
            except json.decoder.JSONDecodeError as e:
                raise LicenseException(
                    f"Failed to parse json: {e}", json_file_path)
            return LicensesClassifications.from_json_list(
                json_obj, json_file_path)

    def _transform_each_classification(
        self, function: Callable[[LicenseClassification], LicenseClassification]
    ) -> "LicensesClassifications":
        """Returns a copy of this object with the classifications transformed by function"""
        new = self.classifications_by_id.copy()
        for k, v in new.items():
            new[k] = function(v)
        return dataclasses.replace(self, classifications_by_id=new)

    def _transform_each_identification(
        self, function: Callable[[IdentifiedSnippet], IdentifiedSnippet]
    ) -> "LicensesClassifications":
        """Returns a copy of this object with the classifications' identified snippets transformed by function"""
        return self._transform_each_classification(
            lambda x: x._transform_identifications(function))

    def set_default_condition(
            self, default_condition: str) -> "LicensesClassifications":
        return self._transform_each_identification(
            lambda x: x.set_condition(default_condition))

    def set_is_shipped_defaults(
            self, is_project_shipped: bool, is_notice_shipped: bool,
            is_source_code_shipped: bool) -> "LicensesClassifications":
        return self._transform_each_identification(
            lambda x: x.set_is_shipped_defaults(
                is_project_shipped, is_notice_shipped, is_source_code_shipped))

    def add_classifications(
            self,
            to_add: List[LicenseClassification]) -> "LicensesClassifications":
        new = self.classifications_by_id.copy()
        for license_classification in to_add:
            license_id = license_classification.license_id
            assert license_id not in new, f"{license_id} already exists"
            new[license_id] = license_classification
        return dataclasses.replace(self, classifications_by_id=new)

    def add_licenses_information(self, spdx_index: SpdxIndex):
        return self._transform_each_classification(
            lambda x: x.add_license_information(spdx_index))

    def compute_identification_stats(self, spdx_index: SpdxIndex):
        return self._transform_each_classification(
            lambda x: x.compute_identification_stats(spdx_index))

    def override_conditions(
            self,
            rule_set: "ConditionOverrideRuleSet") -> "LicensesClassifications":
        return self._transform_each_classification(
            lambda x: x.override_conditions(rule_set))

    def verify_conditions(
            self, allowed_conditions: Set[str]) -> "LicensesClassifications":
        return self._transform_each_classification(
            lambda x: x.verify_conditions(allowed_conditions))

    def verification_errors(self):
        error_messages = []
        for c in self.classifications_by_id.values():
            error_messages.extend(c.verification_errors())
        return error_messages

    def identifications_count(self):
        c = 0
        for v in self.classifications_by_id.values():
            c += len(v.identifications)
        return c

    def failed_verifications_count(self):
        c = 0
        for v in self.classifications_by_id.values():
            for i in v.identifications:
                if not i.verified:
                    c += 1
        return c

    def licenses_count(self):
        return len(self.classifications_by_id)

    def license_ids(self):
        return self.classifications_by_id.keys()


@dataclasses.dataclass(frozen=True)
class AsterixStringExpression:
    """Utility for partial string matching (asterix matches)"""
    starts_with_asterix: bool
    ends_with_asterix: bool
    parts: List[str]

    def create(expression: str) -> "AsterixStringExpression":
        return AsterixStringExpression(
            starts_with_asterix=expression.startswith("*"),
            ends_with_asterix=expression.endswith("*"),
            parts=[p for p in expression.split("*") if p],
        )

    def matches(self, value) -> bool:
        if not self.parts:
            return True
        offset = 0

        if not self.starts_with_asterix and not value.startswith(self.parts[0]):
            return False

        for part in self.parts:
            next_match = value.find(part, offset)
            if next_match == -1:
                return False
            offset = next_match + len(part)

        return offset == len(value) or self.ends_with_asterix


@dataclasses.dataclass(frozen=True)
class StringMatcher:
    """
    A utility to perform override rule string matching.

    Supports exact and * matches.
    """

    all_expressions: List[str]

    exact_expressions: Set[str]
    asterix_expressions: List[AsterixStringExpression]

    def create(expressions: List[str]) -> "StringMatcher":
        assert isinstance(expressions, list)
        exact_expressions = set()
        asterix_expressions = []
        for e in expressions:
            assert isinstance(e, str)
            if "*" in e:
                asterix_expressions.append(AsterixStringExpression.create(e))
            else:
                exact_expressions.add(e)

        return StringMatcher(
            all_expressions=expressions,
            exact_expressions=exact_expressions,
            asterix_expressions=asterix_expressions)

    def create_match_everything() -> "StringMatcher":
        return StringMatcher.create(["*"])

    def to_json(self) -> Any:
        return self.all_expressions

    def matches(self, input: str) -> bool:
        if input in self.exact_expressions:
            return True
        for asterix_expression in self.asterix_expressions:
            if asterix_expression.matches(input):
                return True
        return False

    def get_matches(self, inputs: List[str]) -> List[str]:
        """
        Matches all the inputs against the internal expressions.

        Returns the ones that match or an empty list if none matched.
        """
        return [i for i in inputs if self.matches(i)]

    def matches_any(self, inputs: List[str]) -> bool:
        """
        Matches all the inputs against the internal expressions.

        Returns true if any inputs where matched.
        """
        if not self.all_expressions or not inputs:
            return False
        for input in inputs:
            if self.matches(input):
                return True
        return False

    def matches_all(self, inputs: List[str]) -> bool:
        """
        Matches all the inputs against the internal expressions.

        Returns true if all inputs where matched.
        """
        if not self.all_expressions or not inputs:
            return False

        for input in inputs:
            if not self.matches(input):
                return False
        return True


@dataclasses.dataclass(frozen=True)
class ConditionOverrideRule:
    """Rule for overriding a classified license condition"""

    # Path to the condition override rule.
    rule_file_path: str
    # Will override the condition to this condition
    override_condition_to: str
    # Optional public source mirroring urls.
    public_source_mirrors: List[str]
    # Issue tracker URL.
    bug: str
    # List facilitates easier to read multi-line comments in JSON.
    comment: List[str]

    # matching
    match_license_names: StringMatcher
    match_identifications: StringMatcher
    match_conditions: StringMatcher
    match_dependents: StringMatcher
    match_snippet_checksums: StringMatcher

    def from_json_dict(dictionary, rule_file_path) -> "ConditionOverrideRule":
        if isinstance(dictionary, DictReader):
            reader = dictionary
        else:
            reader = DictReader(dictionary=dictionary, location=rule_file_path)

        override_condition_to = reader.get("override_condition_to")
        bug = reader.get("bug")
        if not bug:
            raise LicenseException(
                "'bug' fields cannot be empty", rule_file_path)
        comment = reader.get_string_list("comment")

        def verify_list_not_empty(list_value) -> str:
            if not list_value:
                return "list is empty"
            for v in list_value:
                if not v:
                    return "empty value in list"
            return None

        criteria_reader = reader.get_reader("match_criteria")

        def read_required_matcher_field(name) -> StringMatcher:
            value = criteria_reader.get(
                name, expected_type=list, verify=verify_list_not_empty)
            return StringMatcher.create(value)

        match_license_names = read_required_matcher_field("license_names")
        match_conditions = read_required_matcher_field("conditions")
        match_dependents = read_required_matcher_field("dependents")
        match_identifications = read_required_matcher_field("identifications")

        # Checksum matching is optional except for unidentified snippets.
        match_snippet_checksums = criteria_reader.get_or(
            "snippet_checksums", expected_type=list, default=None)

        if match_identifications.matches(
                IdentifiedSnippet.UNIDENTIFIED_IDENTIFICATION):
            if not match_snippet_checksums:
                raise LicenseException(
                    f"Rules that match license_names `{IdentifiedSnippet.UNIDENTIFIED_IDENTIFICATION}`"
                    "must also set `snippet_checksum`", rule_file_path)
            if [s for s in match_snippet_checksums if "*" in s]:
                raise LicenseException(
                    "Rules that license_names " \
                    f" `{IdentifiedSnippet.UNIDENTIFIED_IDENTIFICATION}`"\
                    " cannot have `*` expressions in `match_snippet_checksum`",
                    rule_file_path)
        if match_snippet_checksums == None:
            match_snippet_checksums = StringMatcher.create_match_everything()
        else:
            match_snippet_checksums = StringMatcher.create(
                match_snippet_checksums)

        # If there is a rule_file_path value in the dict, use it instead.
        rule_file_path = reader.get_or("rule_file_path", default=rule_file_path)
        public_source_mirrors = reader.get_or(
            "public_source_mirrors", default=None, expected_type=list)

        return ConditionOverrideRule(
            rule_file_path=rule_file_path,
            override_condition_to=override_condition_to,
            public_source_mirrors=public_source_mirrors,
            bug=bug,
            comment=comment,
            match_license_names=match_license_names,
            match_identifications=match_identifications,
            match_conditions=match_conditions,
            match_dependents=match_dependents,
            match_snippet_checksums=match_snippet_checksums,
        )

    def to_json_dict(self):
        # Fields are output in a certain order for better readability
        out = {}
        if self.rule_file_path:
            out["rule_file_path"] = self.rule_file_path

        if self.public_source_mirrors:
            out["public_source_mirrors"] = self.public_source_mirrors

        out.update(
            {
                "override_condition_to": self.override_condition_to,
                "bug": self.bug,
                "comment": self.comment,
                "match_criteria":
                    {
                        "license_names":
                            self.match_license_names.to_json(),
                        "identifications":
                            self.match_identifications.to_json(),
                        "conditions":
                            self.match_conditions.to_json(),
                        "snippet_checksums":
                            self.match_snippet_checksums.to_json(),
                        "dependents":
                            self.match_dependents.to_json(),
                    },
            })
        return out

    def suggested_for_snippet(
            license: LicenseClassification, snippet: IdentifiedSnippet,
            allowed_conditions: Set[str]) -> "ConditionOverrideRule":
        """Creates a an override rule suggestion for the given license snippet"""
        dependents = license.dependents
        if snippet.dependents_unmatched_by_overriding_rules:
            dependents = snippet.dependents_unmatched_by_overriding_rules
        return ConditionOverrideRule(
            rule_file_path=None,
            override_condition_to="<CHOOSE ONE OF " +
            ", ".join([f"'{c}'" for c in allowed_conditions]) + ">",
            public_source_mirrors=None,
            bug="<INSERT TICKET URL>",
            comment=["<INSERT DOCUMENTATION FOR OVERRIDE RULE>"],
            match_license_names=StringMatcher.create([license.name]),
            match_snippet_checksums=StringMatcher.create(
                [snippet.snippet_checksum]),
            match_identifications=StringMatcher.create([snippet.identified_as]),
            match_conditions=StringMatcher.create([snippet.condition]),
            match_dependents=StringMatcher.create(dependents))


@dataclasses.dataclass(frozen=True)
class ConditionOverrideRuleSet:

    rules: List[ConditionOverrideRule]

    def merge(
            self,
            other: "ConditionOverrideRuleSet") -> "ConditionOverrideRuleSet":
        new = list(self.rules)
        new.extend(other.rules)
        return dataclasses.replace(self, rules=new)

    def from_json(file_path: str) -> "ConditionOverrideRuleSet":
        with open(file_path, "r") as f:
            try:
                json_obj = json.load(f)
            except json.decoder.JSONDecodeError as e:
                raise LicenseException(f"Failed to parse json: {e}", file_path)

            if not isinstance(json_obj, list) and not isinstance(json_obj,
                                                                 dict):
                raise LicenseException(
                    f"Expected List[dict] or dict at top-level json but found {type(json_obj)}",
                    file_path)

            if isinstance(json_obj, dict):
                json_obj = [json_obj]

            rules = []
            for child_json in json_obj:
                if not isinstance(child_json, dict):
                    raise LicenseException(
                        f"Expected dict but found {type(child_json)}",
                        file_path)
                rules.append(
                    ConditionOverrideRule.from_json_dict(
                        DictReader(child_json, file_path),
                        rule_file_path=file_path))

            return ConditionOverrideRuleSet(rules)
