"""Set the release version where the repository records it, for publish.yml's by-hand release PR.

    python3 .github/scripts/set_version.py VERSION --changelog-section FILE

Sets the Cargo workspace version and the version of each workspace crate another depends on by
path (a published crate's dependencies must name one). Cargo.lock is left to
`cargo update --workspace`. Inserts the --changelog-section file's text into CHANGELOG.md as the
newest release.

Each file is edited line by line rather than re-serialized, so it stays byte-identical to what
its tool writes apart from the version.
"""

import argparse
import pathlib
import re
import sys
import tomllib

CARGO_TOML = pathlib.Path("Cargo.toml")
CHANGELOG = pathlib.Path("CHANGELOG.md")
# What release-plz writes above the first release (cliff.toml's header).
CHANGELOG_HEADER = (
    "# Changelog\n\nAll notable changes to this project will be documented in this file.\n\n"
)


def set_cargo_version(version: str) -> None:
    text = CARGO_TOML.read_text()
    old = tomllib.loads(text)["workspace"]["package"]["version"]
    # [workspace.package]'s own `version = "…"` line, and each
    # `{ version = "…", path = "crates/…" }`.
    package = re.compile(r'(\[workspace\.package\]\n(?:[^\[\n].*\n)*?version = ")[^"]*(")')
    text, n = package.subn(rf"\g<1>{version}\g<2>", text, count=1)
    if n != 1:
        sys.exit("no version in [workspace.package]")
    path_dep = re.compile(r'(\{ version = ")' + re.escape(old) + r'(", path = "crates/)')
    text = path_dep.sub(rf"\g<1>{version}\g<2>", text)
    CARGO_TOML.write_text(text)
    assert tomllib.loads(text)["workspace"]["package"]["version"] == version


def add_changelog_section(section: str) -> None:
    text = CHANGELOG.read_text() if CHANGELOG.exists() else CHANGELOG_HEADER
    # The section goes above the newest release, below the header.
    first = re.search(r"^## ", text, flags=re.MULTILINE)
    at = first.start() if first else len(text)
    CHANGELOG.write_text(text[:at] + section.strip() + "\n\n" + text[at:])


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("version", help="the new version")
    parser.add_argument(
        "--changelog-section", type=pathlib.Path, required=True, help="the release's section"
    )
    args = parser.parse_args()
    set_cargo_version(args.version)
    add_changelog_section(args.changelog_section.read_text())


if __name__ == "__main__":
    main()
