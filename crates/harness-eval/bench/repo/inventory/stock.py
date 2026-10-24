"""Stock keeping: items, quantities and value."""

from dataclasses import dataclass


@dataclass
class Item:
    sku: str
    name: str
    unit_price_cents: int


class Stock:
    def __init__(self):
        self._items = {}
        self._qty = {}

    def add(self, item, quantity):
        if quantity <= 0:
            raise ValueError("quantity must be positive")
        self._items[item.sku] = item
        self._qty[item.sku] = self._qty.get(item.sku, 0) + quantity

    def remove(self, sku, quantity):
        have = self._qty.get(sku, 0)
        if quantity > have:
            raise ValueError(f"only {have} of {sku} in stock")
        self._qty[sku] = have - quantity

    def quantity(self, sku):
        return self._qty.get(sku, 0)

    def total_value_cents(self):
        total = 0
        for sku, qty in self._qty.items():
            total += self._items[sku].unit_price_cents
        return total

    def skus(self):
        return sorted(self._qty)
