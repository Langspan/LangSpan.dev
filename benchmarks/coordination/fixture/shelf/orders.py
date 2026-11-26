"""Deliveries the shop is waiting for."""

from shelf.dates import days_left


def late_orders(orders):
    """The names of ``orders``, pairs of a name and its due date, whose due
    date has passed, in the order given."""
    return [name for name, due in orders if days_left(due) is None]
