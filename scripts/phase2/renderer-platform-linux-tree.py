#!/usr/bin/python3
"""Pure helpers over the AT-SPI snapshot dictionaries written by the atspi script."""


def contains_role(tree, role):
    """Return True when any node in the snapshot has exactly this AT-SPI role name."""
    if tree.get("role") == role:
        return True
    return any(contains_role(child, role) for child in tree.get("children", []))
