"""Adopted fleet-board lifecycle hook.

Fail-open. The provider's raw payload is reduced to a presence event and is
not written. Install the snippet from `cowork hooks <provider>` rather than
this module; the module remains so an existing hook line can point here.
"""
import sys

from cowork.cli import _hook
from cowork.model import canonical_provider


class _Args:
    def __init__(self, provider, event):
        self.provider = provider
        self.event = event


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    try:
        if not argv or argv[0] == "--flush":
            return 0
        provider = canonical_provider(argv[0])
        event = argv[1]
        return _hook(_Args(provider, event))
    except Exception:
        return 0


if __name__ == "__main__":
    sys.exit(main())
