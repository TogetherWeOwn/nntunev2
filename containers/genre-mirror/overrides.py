"""Apply validated taxonomy overrides to offline root selection."""


class Overrides:
    def __init__(self, config, roots, present):
        self.roots = roots
        self.present = set(present)
        self.actions = {}
        for override in config["overrides"]:
            action = override["action"]
            targets = override.get("roots", [])
            if (action not in ("add", "remove", "replace", "exclude")
                    or type(targets) is not list
                    or any(type(root) is not str or root not in roots.sort_order for root in targets)
                    or len(set(targets)) != len(targets)
                    or bool(targets) == (action == "exclude")):
                raise ValueError("invalid override action or roots")
            actions = self.actions.setdefault(override["genre_qid"], {})
            if action in actions:
                raise ValueError("duplicate override action")
            actions[action] = set(targets)
        for actions in self.actions.values():
            if len(actions) > 1 and (
                "exclude" in actions or "replace" in actions or actions["add"] & actions["remove"]
            ):
                raise ValueError("conflicting override actions")

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
