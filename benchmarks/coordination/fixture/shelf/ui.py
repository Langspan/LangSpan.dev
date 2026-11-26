"""What the shop assistant sees."""

from shelf.settings import DEFAULTS


def list_page(items, page=0):
    """The items shown on listing page ``page`` (counting from 0)."""
    size = DEFAULTS["page_size"]
    return items[page * size:(page + 1) * size]


def health_timeout():
    """Seconds the status light waits for the server's health check."""
    return DEFAULTS["timeout_s"]


def probe_retries():
    """How many times the status light retries a failed health check."""
    return DEFAULTS["retries"]


def price_label(amount):
    """The price shown on a shelf label."""
    return "%.2f %s" % (amount, DEFAULTS["currency"])


def receipt_date(day):
    """The date printed on a receipt."""
    return day.strftime(DEFAULTS["date_format"])
