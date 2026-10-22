import unittest

from calc import average


class AverageTests(unittest.TestCase):
    def test_average(self):
        self.assertEqual(average([2, 4, 6]), 4)

    def test_empty(self):
        self.assertEqual(average([]), 0)


if __name__ == "__main__":
    unittest.main()
