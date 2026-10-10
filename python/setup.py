from setuptools import Distribution, setup


class NativeDistribution(Distribution):
    """Mark the bundled PyO3 extension as a platform-specific binary wheel."""

    def has_ext_modules(self) -> bool:
        return True


setup(
    distclass=NativeDistribution,
    options={"bdist_wheel": {"py_limited_api": "cp310"}},
)
