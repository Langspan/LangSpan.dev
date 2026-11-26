"""Read a shelf configuration from text.

A configuration is one setting per line, ``key = value``. Each ``item``
line describes one stocked item as ``name:quantity:price``. Blank lines and
lines starting with ``#`` are ignored.
"""


def parse_config(text):
    """Return the configuration in ``text`` as a dictionary.

    The result always has an ``items`` list; each item is a dictionary with
    ``name`` (str), ``qty`` (int) and ``price`` (float). Other keys map to
    their string values.
    """
    config = {"items": []}
    for number, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            raise ValueError("line %d: expected key = value" % number)
        key, value = (part.strip() for part in line.split("=", 1))
        if key == "item":
            name, qty, price = value.split(":")
            config["items"].append({"name": name, "qty": int(qty), "price": float(price)})
        else:
            config[key] = value
    return config
