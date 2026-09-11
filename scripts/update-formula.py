#!/usr/bin/env python3
"""Point the Homebrew formula at a released version.

Usage: scripts/update-formula.py VERSION CHECKSUM_FOLDER

VERSION is the release version without the tag's leading `v`. CHECKSUM_FOLDER
holds the `spotify_player-<target>.sha256` files published with the release;
each contains the checksum of the archive it is named after. Checksums for
archives the formula does not reference (the Windows zip) are ignored.

Every url in the formula must get a checksum, otherwise nothing is written: a
formula pointing at a release with unverified archives is worse than a failed
release job.
"""

import pathlib
import re
import sys

FORMULA = pathlib.Path(__file__).resolve().parent.parent / "Formula/spotify_player.rb"


def published_checksums(folder: pathlib.Path) -> dict[str, str]:
    """Map each published archive name to its checksum."""
    files = sorted(folder.glob("*.sha256"))
    if not files:
        sys.exit(f"no *.sha256 files in {folder}")

    checksums = {}
    for file in files:
        fields = file.read_text().split()
        if len(fields) != 2:
            sys.exit(f"{file}: expected '<checksum>  <file>', got {file.read_text()!r}")
        checksum, archive = fields
        checksums[archive] = checksum
    return checksums


def update(formula: str, version: str, checksums: dict[str, str]) -> str:
    formula, count = re.subn(
        r'^(  version ")[^"]*(")', rf"\g<1>{version}\g<2>", formula, flags=re.M
    )
    if count != 1:
        sys.exit(f"expected exactly one version line, found {count}")

    archives = re.findall(r'/(spotify_player-[^/"]+\.tar\.gz)"', formula)
    if not archives:
        sys.exit("no release archives referenced by the formula")

    for archive in archives:
        checksum = checksums.get(archive)
        if checksum is None:
            sys.exit(f"the release published no checksum for {archive}")

        # the sha256 belonging to an archive is on the line following its url
        formula, count = re.subn(
            rf'({re.escape(archive)}"\n\s*sha256 ")[0-9a-f]*(")',
            rf"\g<1>{checksum}\g<2>",
            formula,
        )
        if count != 1:
            sys.exit(f"expected exactly one sha256 for {archive}, found {count}")

    return formula


def main() -> None:
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    version, folder = sys.argv[1], pathlib.Path(sys.argv[2])

    FORMULA.write_text(update(FORMULA.read_text(), version, published_checksums(folder)))
    print(f"updated {FORMULA} to {version}")


if __name__ == "__main__":
    main()
