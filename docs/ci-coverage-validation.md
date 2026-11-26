# Verify selective CI coverage

Open the `ci-commits` job summary for the pull request head. It records the source head, the tested merge commit, the comparison base, and the jobs approved to skip. Use the `ci-ok` result for that same head to verify the plan and the required job outcomes.

For an unreferenced prose change, the code suites can skip while PR lint, the secret scan, the size check, and CodeQL still report independently.
