"""A one-line summary of a shelf configuration."""

from shelf.config import parse_config


def summary(text):
    """Return ``"OWNER: N items"`` for the configuration in ``text``."""
    config = parse_config(text)
    return "%s: %d items" % (config.get("owner", "nobody"), len(config["items"]))
