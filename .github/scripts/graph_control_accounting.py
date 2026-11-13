"""Run the existing graph controls with strict unittest accounting.

This accounts for execution only; it does not establish source provenance.
"""

import contextlib
import io
import json
import sys
import unittest


CONTROLS = (
    "test_preserved_source_graph_workflow.TestPreservedSourceGraphWorkflow",
    "test_migrations_rename_check.GitFixture.test_preserved_source_requires_original_graph_and_data_objects",
)


def run_controls(names=CONTROLS):
    counts = {}
    for name in names:
        suite = unittest.defaultTestLoader.loadTestsFromName(name)
        matched = suite.countTestCases()
        if not matched:
            raise ValueError(f"{name}: no tests matched")
        with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            result = unittest.TextTestRunner(stream=io.StringIO()).run(suite)
        if (result.testsRun != matched or result.failures or result.errors or result.skipped
                or result.expectedFailures or result.unexpectedSuccesses):
            raise ValueError(f"{name}: incomplete or unsuccessful control execution")
        counts[name] = {"matched": matched, "executed": result.testsRun}
    return counts


def main():
    try:
        counts = run_controls()
    except ValueError as exc:
        print(exc, file=sys.stderr)
        return 1
    print(json.dumps(counts, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
