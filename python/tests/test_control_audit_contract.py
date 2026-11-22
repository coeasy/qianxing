"""Control audit response schemas shared by qx-control, qx-api and all SDK clients."""

from __future__ import annotations

import json
import unittest
from pathlib import Path

try:
    import jsonschema
    from referencing import Registry, Resource
except ImportError:  # pragma: no cover - optional developer validation dependency
    jsonschema = None
    Registry = Resource = None


ROOT = Path(__file__).resolve().parents[2]
FIXTURES = Path(__file__).parent / "fixtures"


def _document(path: Path):
    return json.loads(path.read_text(encoding="utf-8"))


class ControlAuditContractTests(unittest.TestCase):
    @unittest.skipIf(
        jsonschema is None or Registry is None,
        "install jsonschema and referencing to validate registered wire schemas",
    )
    def test_record_and_bounded_page_schemas_accept_the_shared_golden_fixtures(self):
        record_schema = _document(ROOT / "schemas" / "control-audit-record-v1.json")
        page_schema = _document(ROOT / "schemas" / "control-audit-page-v1.json")
        record = _document(FIXTURES / "control-audit-record-v1.json")
        page = _document(FIXTURES / "control-audit-page-v1.json")

        jsonschema.Draft202012Validator.check_schema(record_schema)
        jsonschema.Draft202012Validator.check_schema(page_schema)
        record_validator = jsonschema.Draft202012Validator(record_schema)
        registry = Registry().with_resource(
            record_schema["$id"], Resource.from_contents(record_schema)
        )
        page_validator = jsonschema.Draft202012Validator(page_schema, registry=registry)
        self.assertEqual(list(record_validator.iter_errors(record)), [])
        self.assertEqual(list(page_validator.iter_errors(page)), [])
        self.assertEqual(
            page_schema["properties"]["entries"]["items"]["properties"]["record"]["$ref"],
            record_schema["$id"],
        )

        for mutate in (
            lambda value: value.update(schema_version=2),
            lambda value: value.update(command_id=True),
            lambda value: value.update(status="Queued"),
            lambda value: value.update(extra=True),
        ):
            invalid = dict(record)
            mutate(invalid)
            with self.subTest(invalid=invalid):
                self.assertTrue(list(record_validator.iter_errors(invalid)))

        for mutate in (
            lambda value: value.update(schema_version=2),
            lambda value: value.update(has_more=True),
            lambda value: value.update(extra=True),
            lambda value: value.update(snapshot_through_sequence=-1),
        ):
            invalid = dict(page)
            mutate(invalid)
            with self.subTest(invalid_page=invalid):
                self.assertTrue(list(page_validator.iter_errors(invalid)))


if __name__ == "__main__":
    unittest.main()
