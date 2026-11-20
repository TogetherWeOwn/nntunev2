"""Apply validated taxonomy overrides to offline root selection."""


class Overrides:
    def __init__(self, config, roots, present):
        self.roots = roots
        self.present = set(present)
        self.actions = {}
        for override in config["overrides"]:
            self.actions.setdefault(override["genre_qid"], {})[override["action"]] = set(
                override.get("roots", [])
            )

    def resolve(self, genre_qid):
        """Return ordered roots, cap, cycle, and missing-target status."""
        actions = self.actions.get(genre_qid, {})
        missing = bool(actions) and genre_qid not in self.present
        if missing:
            actions = {}
        if "exclude" in actions:
            return [], False, False, missing
        if "replace" in actions:
            return sorted(actions["replace"], key=self.roots.sort_order.__getitem__), False, False, missing
        found, capped, cycle = self.roots.resolve(genre_qid)
        selected = (set(found) | actions.get("add", set())) - actions.get("remove", set())
        return sorted(selected, key=self.roots.sort_order.__getitem__), capped, cycle, missing
