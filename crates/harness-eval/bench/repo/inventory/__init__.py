"""A tiny inventory library (harness bench fixture)."""

from .stock import Item, Stock
from .report import format_report

__all__ = ["Item", "Stock", "format_report"]
