import unittest

from inventory import Item, Stock


def sample():
    s = Stock()
    s.add(Item("A1", "bolt", 25), 4)
    s.add(Item("B2", "nut", 10), 3)
    return s


class StockTest(unittest.TestCase):
    def test_quantity(self):
        self.assertEqual(sample().quantity("A1"), 4)

    def test_remove(self):
        s = sample()
        s.remove("A1", 1)
        self.assertEqual(s.quantity("A1"), 3)

    def test_remove_too_many(self):
        with self.assertRaises(ValueError):
            sample().remove("B2", 9)

    def test_total_value(self):
        self.assertEqual(sample().total_value_cents(), 130)


if __name__ == "__main__":
    unittest.main()
