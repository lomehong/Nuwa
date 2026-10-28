import json
import sys


def average(values):
    if not values:
        return 0
    return sum(values) / len(values)


if __name__ == "__main__":
    print(json.dumps(average(json.loads(sys.stdin.read()))))