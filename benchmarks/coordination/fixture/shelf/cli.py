"""Command line entry point: ``python3 -m shelf.cli FILE`` prints each item."""

import sys

from shelf import config


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if len(argv) != 1:
        print("usage: python3 -m shelf.cli FILE", file=sys.stderr)
        return 2
    with open(argv[0], encoding="utf-8") as source:
        parsed = config.parse_config(source.read())
    for item in parsed["items"]:
        print("%s\t%d\t%.2f" % (item["name"], item["qty"], item["price"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
