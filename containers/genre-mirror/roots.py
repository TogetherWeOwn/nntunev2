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
        self.anchors_capped = False
        for root in active:
            found = set()
            for anchor in root["anchors"]:
                visited, capped, _ = graph.reach(anchor)
                self.anchors_capped |= capped
                found.update(self.anchor_root[node] for node in visited if node in self.anchor_root)
            reaches[root["root_id"]] = found

        # Compute cycles over the full projected relation, including mutual
        # edges. Multiple anchors can join otherwise separate paths into a cycle.
        closure = {}
        for rid in reaches:
            seen, pending = set(), list(reaches[rid])
            while pending:
                other = pending.pop()
                if other not in seen:
                    seen.add(other)
                    pending.extend(reaches[other])
            closure[rid] = seen
        # A capped anchor can hide an edge anywhere in a projected cycle.
        # Preserve candidates rather than treating missing edges as proof.
        self.narrower = {
            rid: {other for other in broader
                  if not self.anchors_capped and other != rid and rid not in closure[other]}
            for rid, broader in reaches.items()
        }

    def resolve(self, genre_qid):
        """Return sorted roots, any anchor/query cap, and query cycle status."""
        visited, capped, cycle = self.graph.reach(genre_qid)
        candidates = {self.anchor_root[node] for node in visited if node in self.anchor_root}
        reduced = {
            broader for broader in candidates
            if not any(broader in self.narrower[narrower] for narrower in candidates)
        }
        return sorted(reduced, key=self.sort_order.__getitem__), capped or self.anchors_capped, cycle
