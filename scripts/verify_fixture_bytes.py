"""Check the repository fixtures against the published DuckDB starter."""

import argparse
from pathlib import Path, PurePosixPath
from zipfile import ZipFile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("starter", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1] / "fixtures"
    prefix = "starter-duckdb/fixtures/"

    with ZipFile(args.starter) as archive:
        expected = {
            PurePosixPath(item.filename.removeprefix(prefix)): item
            for item in archive.infolist()
            if item.filename.startswith(prefix) and not item.is_dir()
        }
        present = {PurePosixPath(path.relative_to(root).as_posix()) for path in root.rglob("*") if path.is_file()}
        if not expected or present != expected.keys():
            raise SystemExit("Fixture filenames differ from the published archive.")
        for path, item in expected.items():
            if root.joinpath(*path.parts).read_bytes() != archive.read(item):
                raise SystemExit(f"Fixture bytes differ: {path}")

    print(f"All {len(expected)} fixture files match the published archive byte for byte.")


if __name__ == "__main__":
    main()
