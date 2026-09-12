from setuptools import setup
from wheel.bdist_wheel import bdist_wheel


class BinaryWheel(bdist_wheel):
    """Mark wheels containing the PyO3 extension as platform-specific."""

    def finalize_options(self):
        super().finalize_options()
        self.root_is_pure = False


setup(cmdclass={"bdist_wheel": BinaryWheel})
