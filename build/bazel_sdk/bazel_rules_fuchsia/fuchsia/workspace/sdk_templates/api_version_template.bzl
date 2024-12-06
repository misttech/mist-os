"""
Generated internal values related to Fuchsia Platform Versioning and API levels.

Do not use these values directly as the structure may change over time.
Instead, use the supported methods to access API levels.
"""

INTERNAL_ONLY_VALID_TARGET_APIS = [{{valid_target_apis}}]

API_STATUS = struct(
    SUPPORTED = "supported",
    UNSUPPORTED = "unsupported",
    IN_DEVELOPMENT = "in-development",
)
