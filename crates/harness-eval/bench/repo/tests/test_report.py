import unittest

from inventory import Item, Stock, format_report


class ReportTest(unittest.TestCase):
    def test_report(self):
        s = Stock()
        s.add(Item("A1", "bolt", 25), 4)
        self.assertEqual(format_report(s), "A1: 4\ntotal: $1.00")


if __name__ == "__main__":
    unittest.main()
