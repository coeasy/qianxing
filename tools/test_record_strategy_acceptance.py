import unittest

from tools.record_strategy_acceptance import redact_bytes, redact_command, redact_text


class AcceptanceEvidenceRedactionTests(unittest.TestCase):
    def test_redacts_secret_environment_assignment_and_database_url(self) -> None:
        command = redact_command(
            ["podman", "run", "-e", "QX_TEST_POSTGRES_DSN=postgres://postgres:pw@db/test"]
        )
        self.assertEqual(
            command,
            ["podman", "run", "-e", "QX_TEST_POSTGRES_DSN=<redacted>"],
        )

    def test_redacts_separate_secret_flags_and_bearer_output(self) -> None:
        self.assertEqual(
            redact_command(["client", "--api-key", "private-value"]),
            ["client", "--api-key", "<redacted>"],
        )
        self.assertEqual(
            redact_text("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9"),
            "Authorization: <redacted>",
        )
        self.assertEqual(
            redact_text('runner output: {"access_token": "private-value"}'),
            'runner output: {"access_token": "<redacted>"}',
        )
        self.assertEqual(
            redact_text("response Authorization: Bearer private-value"),
            "response Authorization: <redacted>",
        )

    def test_preserves_non_secret_arguments_and_redacts_captured_bytes(self) -> None:
        self.assertEqual(
            redact_command(["cargo", "test", "--locked"]),
            ["cargo", "test", "--locked"],
        )
        self.assertEqual(
            redact_bytes(b"DB_PASSWORD=local-pass\n"),
            b"DB_PASSWORD=<redacted>\n",
        )


if __name__ == "__main__":
    unittest.main()
