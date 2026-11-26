"""Sending the shelf to the warehouse system."""

from shelf.settings import DEFAULTS


def export_batch(items):
    """Split ``items`` into the batches one export request sends."""
    size = DEFAULTS["page_size"]
    return [items[start:start + size] for start in range(0, len(items), size)]


def upload_timeout():
    """Seconds one upload request may take."""
    return DEFAULTS["timeout_s"]


def sync_retries():
    """How many times a failed warehouse sync is retried."""
    return DEFAULTS["retries"]


def invoice_total(amount):
    """The total line of a warehouse invoice."""
    return "%.2f %s" % (amount, DEFAULTS["currency"])


def export_filename(day):
    """The file name of the export for the date ``day``."""
    return "export-%s.csv" % day.strftime(DEFAULTS["date_format"])
