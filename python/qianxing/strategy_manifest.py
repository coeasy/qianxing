"""Compatibility facade for the dependency-light strategy metadata contract.

The canonical implementation lives with strategy adapters so worker startup does
not import the application SDK, HTTP clients, SSL, or the native extension.
"""
from qianxing_strategy.manifest import (
    declared_config_validator,
    normalize_manifest,
    bind_code_identity,
    apply_parameter_defaults,
    resolve_strategy_parameters,
    prepare_runtime_strategy_spec,
    build_module_manifest,
)

__all__ = ['declared_config_validator', 'normalize_manifest', 'bind_code_identity', 'apply_parameter_defaults', 'resolve_strategy_parameters', 'prepare_runtime_strategy_spec', 'build_module_manifest']
