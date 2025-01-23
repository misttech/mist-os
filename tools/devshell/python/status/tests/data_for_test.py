# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This file is used to generate the JSON output.
ARGS_GN_BASE = """
import("//boards/x64.gni")
import("//products/core.gni")

# Basic args:
compilation_mode = "debug"

# Target lists:
base_package_labels = []
cache_package_labels = []
universe_package_labels = [
  "//scripts:tests",
  "//tools/devshell/python:tests",
]
base_package_labels += ["//src:tests", "//other:tests"]
base_package_labels -= ["//src:tests"]
cache_package_labels = ["//src/other:tests"]
"""

# This JSON output results from running `gn format --dump-tree=json`
# on the above content. If you change the base template, make sure
# to update this value.
ARGS_GN_JSON = r"""
{
   "begin_token": "",
   "child": [ {
      "child": [ {
         "begin_token": "(",
         "child": [ {
            "location": {
               "begin_column": 8,
               "begin_line": 1,
               "end_column": 26,
               "end_line": 1
            },
            "type": "LITERAL",
            "value": "\"//boards/x64.gni\""
         } ],
         "end": {
            "location": {
               "begin_column": 26,
               "begin_line": 1,
               "end_column": 27,
               "end_line": 1
            },
            "type": "END",
            "value": ")"
         },
         "location": {
            "begin_column": 7,
            "begin_line": 1,
            "end_column": 26,
            "end_line": 1
         },
         "type": "LIST"
      } ],
      "location": {
         "begin_column": 1,
         "begin_line": 1,
         "end_column": 26,
         "end_line": 1
      },
      "type": "FUNCTION",
      "value": "import"
   }, {
      "child": [ {
         "begin_token": "(",
         "child": [ {
            "location": {
               "begin_column": 8,
               "begin_line": 2,
               "end_column": 29,
               "end_line": 2
            },
            "type": "LITERAL",
            "value": "\"//products/core.gni\""
         } ],
         "end": {
            "location": {
               "begin_column": 29,
               "begin_line": 2,
               "end_column": 30,
               "end_line": 2
            },
            "type": "END",
            "value": ")"
         },
         "location": {
            "begin_column": 7,
            "begin_line": 2,
            "end_column": 29,
            "end_line": 2
         },
         "type": "LIST"
      } ],
      "location": {
         "begin_column": 1,
         "begin_line": 2,
         "end_column": 29,
         "end_line": 2
      },
      "type": "FUNCTION",
      "value": "import"
   }, {
      "before_comment": [ "# Basic args:" ],
      "child": [ {
         "location": {
            "begin_column": 1,
            "begin_line": 5,
            "end_column": 17,
            "end_line": 5
         },
         "type": "IDENTIFIER",
         "value": "compilation_mode"
      }, {
         "location": {
            "begin_column": 20,
            "begin_line": 5,
            "end_column": 27,
            "end_line": 5
         },
         "type": "LITERAL",
         "value": "\"debug\""
      } ],
      "location": {
         "begin_column": 1,
         "begin_line": 5,
         "end_column": 27,
         "end_line": 5
      },
      "type": "BINARY",
      "value": "="
   }, {
      "before_comment": [ "# Target lists:" ],
      "child": [ {
         "location": {
            "begin_column": 1,
            "begin_line": 8,
            "end_column": 20,
            "end_line": 8
         },
         "type": "IDENTIFIER",
         "value": "base_package_labels"
      }, {
         "begin_token": "[",
         "child": [  ],
         "end": {
            "location": {
               "begin_column": 24,
               "begin_line": 8,
               "end_column": 25,
               "end_line": 8
            },
            "type": "END",
            "value": "]"
         },
         "location": {
            "begin_column": 23,
            "begin_line": 8,
            "end_column": 24,
            "end_line": 8
         },
         "type": "LIST"
      } ],
      "location": {
         "begin_column": 1,
         "begin_line": 8,
         "end_column": 24,
         "end_line": 8
      },
      "type": "BINARY",
      "value": "="
   }, {
      "child": [ {
         "location": {
            "begin_column": 1,
            "begin_line": 9,
            "end_column": 21,
            "end_line": 9
         },
         "type": "IDENTIFIER",
         "value": "cache_package_labels"
      }, {
         "begin_token": "[",
         "child": [  ],
         "end": {
            "location": {
               "begin_column": 25,
               "begin_line": 9,
               "end_column": 26,
               "end_line": 9
            },
            "type": "END",
            "value": "]"
         },
         "location": {
            "begin_column": 24,
            "begin_line": 9,
            "end_column": 25,
            "end_line": 9
         },
         "type": "LIST"
      } ],
      "location": {
         "begin_column": 1,
         "begin_line": 9,
         "end_column": 25,
         "end_line": 9
      },
      "type": "BINARY",
      "value": "="
   }, {
      "child": [ {
         "location": {
            "begin_column": 1,
            "begin_line": 10,
            "end_column": 24,
            "end_line": 10
         },
         "type": "IDENTIFIER",
         "value": "universe_package_labels"
      }, {
         "begin_token": "[",
         "child": [ {
            "location": {
               "begin_column": 3,
               "begin_line": 11,
               "end_column": 20,
               "end_line": 11
            },
            "type": "LITERAL",
            "value": "\"//scripts:tests\""
         }, {
            "location": {
               "begin_column": 3,
               "begin_line": 12,
               "end_column": 34,
               "end_line": 12
            },
            "type": "LITERAL",
            "value": "\"//tools/devshell/python:tests\""
         } ],
         "end": {
            "location": {
               "begin_column": 1,
               "begin_line": 13,
               "end_column": 2,
               "end_line": 13
            },
            "type": "END",
            "value": "]"
         },
         "location": {
            "begin_column": 27,
            "begin_line": 10,
            "end_column": 1,
            "end_line": 13
         },
         "type": "LIST"
      } ],
      "location": {
         "begin_column": 1,
         "begin_line": 10,
         "end_column": 1,
         "end_line": 13
      },
      "type": "BINARY",
      "value": "="
   }, {
      "child": [ {
         "location": {
            "begin_column": 1,
            "begin_line": 14,
            "end_column": 20,
            "end_line": 14
         },
         "type": "IDENTIFIER",
         "value": "base_package_labels"
      }, {
         "begin_token": "[",
         "child": [ {
            "location": {
               "begin_column": 25,
               "begin_line": 14,
               "end_column": 38,
               "end_line": 14
            },
            "type": "LITERAL",
            "value": "\"//src:tests\""
         }, {
            "location": {
               "begin_column": 40,
               "begin_line": 14,
               "end_column": 55,
               "end_line": 14
            },
            "type": "LITERAL",
            "value": "\"//other:tests\""
         } ],
         "end": {
            "location": {
               "begin_column": 55,
               "begin_line": 14,
               "end_column": 56,
               "end_line": 14
            },
            "type": "END",
            "value": "]"
         },
         "location": {
            "begin_column": 24,
            "begin_line": 14,
            "end_column": 55,
            "end_line": 14
         },
         "type": "LIST"
      } ],
      "location": {
         "begin_column": 1,
         "begin_line": 14,
         "end_column": 55,
         "end_line": 14
      },
      "type": "BINARY",
      "value": "+="
   }, {
      "child": [ {
         "location": {
            "begin_column": 1,
            "begin_line": 15,
            "end_column": 20,
            "end_line": 15
         },
         "type": "IDENTIFIER",
         "value": "base_package_labels"
      }, {
         "begin_token": "[",
         "child": [ {
            "location": {
               "begin_column": 25,
               "begin_line": 15,
               "end_column": 38,
               "end_line": 15
            },
            "type": "LITERAL",
            "value": "\"//src:tests\""
         } ],
         "end": {
            "location": {
               "begin_column": 38,
               "begin_line": 15,
               "end_column": 39,
               "end_line": 15
            },
            "type": "END",
            "value": "]"
         },
         "location": {
            "begin_column": 24,
            "begin_line": 15,
            "end_column": 38,
            "end_line": 15
         },
         "type": "LIST"
      } ],
      "location": {
         "begin_column": 1,
         "begin_line": 15,
         "end_column": 38,
         "end_line": 15
      },
      "type": "BINARY",
      "value": "-="
   }, {
      "child": [ {
         "location": {
            "begin_column": 1,
            "begin_line": 16,
            "end_column": 21,
            "end_line": 16
         },
         "type": "IDENTIFIER",
         "value": "cache_package_labels"
      }, {
         "begin_token": "[",
         "child": [ {
            "location": {
               "begin_column": 25,
               "begin_line": 16,
               "end_column": 44,
               "end_line": 16
            },
            "type": "LITERAL",
            "value": "\"//src/other:tests\""
         } ],
         "end": {
            "location": {
               "begin_column": 44,
               "begin_line": 16,
               "end_column": 45,
               "end_line": 16
            },
            "type": "END",
            "value": "]"
         },
         "location": {
            "begin_column": 24,
            "begin_line": 16,
            "end_column": 44,
            "end_line": 16
         },
         "type": "LIST"
      } ],
      "location": {
         "begin_column": 1,
         "begin_line": 16,
         "end_column": 44,
         "end_line": 16
      },
      "type": "BINARY",
      "value": "="
   } ],
   "location": {
      "begin_column": 1,
      "begin_line": 1,
      "end_column": 44,
      "end_line": 16
   },
   "result_mode": "discards_result",
   "type": "BLOCK"
}
"""
