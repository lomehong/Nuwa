import unittest

from calc import double


class DoubleTests(unittest.TestCase):
    def test_double(self):
        self.assertEqual(double(3), 6)


if __name__ == "__main__":
    unittest.main()
