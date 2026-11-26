"""Items that need restocking."""

import shelf.config


def restock_list(text, minimum=5):
    """Return the names of items with fewer than ``minimum`` in stock."""
    items = shelf.config.parse_config(text)["items"]
    return [item["name"] for item in items if item["qty"] < minimum]
