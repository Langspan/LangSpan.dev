# Shelf

A small stock-keeping package. It needs Python 3.9 or later and nothing else.

- `shelf/config.py` reads a configuration: one `key = value` setting per line,
  and one `item = name:quantity:price` line per stocked item.
- `shelf/report.py`, `shelf/restock.py` and `shelf/cli.py` use that parser.
- `shelf/settings.py` holds the defaults that `shelf/export.py` and
  `shelf/ui.py` read.
- `shelf/dates.py` counts the days left until a date; `shelf/orders.py` uses
  it.
- `shelf/features/` holds small features, one module each.

Run the tests from this directory:

```sh
python3 -m unittest discover -s tests
```
