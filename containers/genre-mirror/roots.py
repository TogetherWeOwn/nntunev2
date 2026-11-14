"""Resolve reached genre anchors to the most-specific active roots.

The caller supplies a validated taxonomy and a bounded Graph. This module is
purely offline and does not load or persist catalogue data.
"""


class Roots:
    def __init__(self, config, graph):
        self.graph = graph
        active = [root for root in config["roots"] if not root.get("retired", False)]
        self.sort_order = {root["root_id"]: root["sort_order"] for root in active}
        self.anchor_root = {
            anchor: root["root_id"] for root in active for anchor in root["anchors"]
        }
        reaches = {}
        for root in active:
            found = set()
            for anchor in root["anchors"]:
                visited, _, _ = graph.reach(anchor)
                found.update(self.anchor_root[node] for node in visited if node in self.anchor_root)
            reaches[root["root_id"]] = found

        narrower = {
            rid: {other for other in reaches[rid]
                  if other != rid and rid not in reaches[other]}
            for rid in reaches
        }
        # Multiple anchors may project an acyclic genre graph into a root cycle.
        # Roots in that cycle are incomparable, so do not discard all of them.
        closure = {}
        for rid in narrower:
            seen, pending = set(), list(narrower[rid])
            while pending:
                other = pending.pop()
                if other not in seen:
                    seen.add(other)
                    pending.extend(narrower[other])
            closure[rid] = seen
        self.narrower = {
            rid: {other for other in broader if rid not in closure[other]}
            for rid, broader in narrower.items()
        }

    def resolve(self, genre_qid):
        """Return sorted root IDs, traversal-capped status, and cycle status."""
        visited, capped, cycle = self.graph.reach(genre_qid)
        candidates = {self.anchor_root[node] for node in visited if node in self.anchor_root}
        reduced = {
            broader for broader in candidates
            if not any(broader in self.narrower[narrower] for narrower in candidates)
        }
        return sorted(reduced, key=self.sort_order.__getitem__), capped, cycle
