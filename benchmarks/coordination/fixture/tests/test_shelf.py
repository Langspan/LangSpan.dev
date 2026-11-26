import datetime
import unittest

from shelf import cli, export, orders, report, restock, ui
from shelf.settings import DEFAULTS

SAMPLE = """\
# A small shop
owner = Corner Shop
item = apples:12:0.50
item = pears:3:0.75
item = plums:0:1.20
"""


class ConfigUsers(unittest.TestCase):
    def test_summary_counts_items(self):
        self.assertEqual(report.summary(SAMPLE), "Corner Shop: 3 items")

    def test_restock_list_names_low_items(self):
        self.assertEqual(restock.restock_list(SAMPLE), ["pears", "plums"])

    def test_cli_rejects_missing_file_argument(self):
        self.assertEqual(cli.main([]), 2)


class SettingsUsers(unittest.TestCase):
    def test_batches_and_pages_cover_every_item(self):
        items = list(range(45))
        self.assertEqual(sum(len(batch) for batch in export.export_batch(items)), 45)
        self.assertEqual(ui.list_page(items, 0), items[:len(ui.list_page(items, 0))])

    def test_amounts_and_dates_are_formatted(self):
        self.assertTrue(export.invoice_total(2).startswith("2.00 "))
        self.assertTrue(ui.price_label(1.5).startswith("1.50 "))
        day = datetime.date(2026, 1, 2)
        self.assertIn("2026", export.export_filename(day))
        self.assertIn("2026", ui.receipt_date(day))

    def test_every_default_is_set(self):
        self.assertEqual(
            sorted(DEFAULTS), ["currency", "date_format", "page_size", "retries", "timeout_s"])


class DateUsers(unittest.TestCase):
    def test_late_orders_are_past_their_due_date(self):
        today = datetime.date.today()
        due = [("flour", today - datetime.timedelta(days=1)), ("salt", today),
               ("sugar", today + datetime.timedelta(days=2))]
        self.assertEqual(orders.late_orders(due), ["flour"])


if __name__ == "__main__":
    unittest.main()
