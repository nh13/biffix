"""Print one string field of the JSON object on stdin.

Usage: json_field.py <key>

The key is an argument rather than interpolated into a program's text, and malformed input, a
non-object, a missing key or a non-string value all print an empty value instead of a traceback.
"""

import json
import sys


def field(text: str, key: str) -> str:
    try:
        data = json.loads(text)
    except ValueError:
        return ""
    if not isinstance(data, dict):
        return ""
    value = data.get(key, "")
    return value if isinstance(value, str) else ""


if __name__ == "__main__":
    print(field(sys.stdin.read(), sys.argv[1]))
