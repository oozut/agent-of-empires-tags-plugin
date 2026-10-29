"""The llm-tags resolver.

One implementation of "what does this session resolve to", shared by every CLI
verb (and, from M1, by the worker). Stdlib only, no aoe imports: the only aoe
dependency is a subprocess call to `aoe session show --json`, made through an
injectable runner so the tests need neither aoe nor a network.

Modules:
    settings  — where settings live and how a profile overrides the global file
    resolver  — session facts, rules, template rendering, env-name derivation
    cli       — argument parsing for bin/aoe-llm-tags
"""

__all__ = ["cli", "resolver", "settings"]
