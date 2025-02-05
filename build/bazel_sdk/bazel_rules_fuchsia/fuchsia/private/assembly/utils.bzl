# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Utility functions used by multiple bazel rules and macros."""

load("@bazel_skylib//lib:paths.bzl", "paths")

# A dictionary to be expanded inside a ctx.actions.run() or
# ctx.actions.run_shell() call to specify that the corresponding
# action should only run locally.
#
# Example usage is:
#
#    ctx.actions.run(
#      executable = ...,
#      inputs = ...,
#      outputs = ....
#      **LOCAL_ONLY_ACTION_KWARGS
#    )
#
# A good reason to use this is to avoid sending very large
# input or outputs through the network, especially when
# running the command locally can still be fast.
#
# IMPORTANT: This does NOT disable Bazel sandboxing, like
# the Bazel "local" tag does.
#
# See https://bazel.build/reference/be/common-definitions#common-attributes
#
LOCAL_ONLY_ACTION_KWARGS = {
    "execution_requirements": {
        "no-remote": "1",
        "no-cache": "1",
    },
}

def select_root_dir(files):
    """Finds the top-most directory in a set of files.

    Args:
      files: A list of files.

    Returns:
      The top-most directory.
    """
    shortest = paths.dirname(files[0].path)
    for file in files:
        directory = paths.dirname(file.path)
        if len(directory) < len(shortest):
            shortest = directory
    return shortest

def select_single_file(files, basename, error_footer = ""):
    """Finds a single file with a given basename. Multiple matches will fail.

    Args:
      files: A list of files.
      basename: The basename of the desired file.
      error_footer: Optionally adds a non-generic error message footer.

    Returns:
      The single file matching the basename.
      It's guaranteed that exactly one file matches that basename.
    """
    matching_files = [file for file in files if file.basename == basename]

    if not matching_files:
        NO_MATCHING_FILE = "\n\nCould not find {} in {}.\n{}".format(
            basename,
            files,
            error_footer,
        ).rstrip() + "\n\n"
        fail(NO_MATCHING_FILE)

    if len(matching_files) > 1:
        AMBIGUOUS_FILE_MATCH = "\n\nToo many matches of {} in {}. (Multiple matches are not allowed).\n{}".format(
            basename,
            files,
            error_footer,
        ).rstrip() + "\n\n"
        fail(AMBIGUOUS_FILE_MATCH)

    return matching_files[0]

def select_multiple_files(files, basename, error_footer = ""):
    """Finds all files that match the given basename. Zero matches will fail.

    Args:
      files: A list of files.
      basename: The basename of the desired files.
      error_footer: Optionally adds a non-generic error message footer.

    Returns:
      A list of files matching the basename.
      It's guaranteed that this list is non-empty.
    """
    matching_files = [file for file in files if file.basename == basename]

    if not matching_files:
        # Assign error string to variable for a cleaner stack trace.
        NO_MATCHING_FILES = "\n\nCould not find any {} in {}.\n{}".format(
            basename,
            files,
            error_footer,
        ).rstrip() + "\n\n"
        fail(NO_MATCHING_FILES)

    return matching_files

def extract_labels(json_dict):
    """Walk json_dict and return a map of all the labels found.

    Args:
      json_dict: starlark dictionary to extract labels from

    Returns:
        A map of the label to LABEL(label) strings.
    """
    extracted_raw_config_labels = {}

    # buildifier: disable=unused-variable
    def _extract_labels_visitor(dictionary, key, value):
        if type(value) == "string" and value.startswith("LABEL("):
            if not value.endswith(")"):
                fail("Syntax error: LABEL does not have closing bracket")
            label = value[6:-1]
            extracted_raw_config_labels[label] = value

    def _remove_none_values_visitor(dictionary, key, value):
        """Remove keys with a value of None.

        Some optional keys will necessarily be supplied as 'None' value instead
        of being omitted entirely, because Bazel doesn't allow the use of top-
        level 'if' statements, instead, they can only be used within the value
        of an expression:

          "foo": value if foo else None

        However, we want to strip those 'None' values before generating the json
        from the nested dicts.
        """
        if value == None:
            dictionary.pop(key)

    _walk_json(json_dict, [_remove_none_values_visitor, _extract_labels_visitor])
    return extracted_raw_config_labels

def replace_labels_with_files(json_dict, target_to_string_map, relative = None):
    """Replace all labels in json_dict with file paths.

    Uses target_to_string_map to find the labels to replace.
    Note that this function cannot be merged with extract_labels(), because we
    need to pass the labels into a Bazel rule with label_keyed_string_dict so
    that Bazel can convert the labels to targets. That is the only way we can
    get the file paths from a label.

    Args:
      json_dict: starlark dictionary with label strings
      target_to_string_map: starlark dictionary mapping the Target to the
        LABEL(label) strings
      relative: if provided, remove the given directory prefix in order to refer
        to files under the given directory by relative paths
    """

    # Invert the map so that it is LABEL(label) to label.
    string_to_target_map = {}
    for label, string in target_to_string_map.items():
        string_to_target_map[string] = label

    # Replace each LABEL(label) with the file path.
    def _replace_labels_visitor(dictionary, key, value):
        # buildifier: disable=uninitialized
        if type(value) == "string" and value in string_to_target_map:
            label = string_to_target_map.get(value)
            label_files = label.files.to_list()
            if relative:
                dictionary[key] = paths.relativize(label_files[0].path, relative)
            else:
                dictionary[key] = label_files[0].path

    _walk_json(json_dict, [_replace_labels_visitor])

def _walk_json(json_dict, visit_node_funcs):
    """Walks a json dictionary, applying the functions in `visit_node_funcs` on every node.

    Args:
        json_dict: The dictionary to walk.
        visit_node_funcs: A function that takes 3 arguments: dictionary, key, value.
    """
    nodes_to_visit = []

    def _enqueue(dictionary, k, v):
        nodes_to_visit.append(struct(
            dictionary = dictionary,
            key = k,
            value = v,
        ))

    def _enqueue_dictionary_children(dictionary):
        for key, value in dictionary.items():
            _enqueue(dictionary, key, value)

    def _enqueue_array(array):
        for i, value in enumerate(array):
            _enqueue(array, i, value)

    _enqueue_dictionary_children(json_dict)

    # Bazel doesn't support recursion, but a json object will always have less
    # nodes than number of serialized characters, so this iteration suffices.
    max_nodes = len(str(json_dict))
    for _unused in range(0, max_nodes):
        if not nodes_to_visit:
            break
        node = nodes_to_visit.pop()
        for visit_node_func in visit_node_funcs:
            visit_node_func(dictionary = node.dictionary, key = node.key, value = node.value)
        if type(node.value) == "dict":
            _enqueue_dictionary_children(node.value)
        if type(node.value) == "list":
            _enqueue_array(node.value)

def combine_directories(ctx, src, dst, inputs, outputs, subdirectories_to_overwrite = None):
    """Copies a directory to the outdir, replacing the specified subdirectories.

    This is done all as one action to avoid the problem of having actions with
    outputs inside of a declared directory.

    Args:
        ctx: the bazel context
        src: top-level directory to copy
        dst: destination
        inputs: inputs for the action
        outputs: outputs of the action. This is usually a declared directory.
        subdirectories_to_overwrite: a dict where the keys are source subdirectories
            and the values are the names of the directories under the top-level directory
            which are to be replaced.
    """
    cmd = """\
if [ ! -d \"$1\" ]; then
    echo \"Error: $1 is not a directory\"
    exit 1
fi

rm -rf \"$2\" && cp -fR \"$1/\" \"$2\";
"""
    if subdirectories_to_overwrite:
        for src_subdir, dest_subdir in subdirectories_to_overwrite.items():
            # Check that all subdirectories which are being overwritten exist
            dest_dir = dst + "/" + dest_subdir
            cmd += 'if [ ! -d "' + dest_dir + '" ]; then echo "ERROR: ' + dest_dir + ' does not exist." 1>&2; exit 1; fi;\n'
            cmd += 'rm -rf \"' + dst + "/" + dest_subdir + '\";\n'
            cmd += 'cp -fR \"' + src_subdir + "\" \"" + dst + "/" + dest_subdir + '\";\n'
    mnemonic = "CopyDirectory"
    progress_message = "Copying directory %s" % src

    ctx.actions.run_shell(
        inputs = inputs,
        outputs = outputs,
        command = cmd,
        arguments = [src, dst],
        mnemonic = mnemonic,
        progress_message = progress_message,
        use_default_shell_env = True,
        **LOCAL_ONLY_ACTION_KWARGS
    )
