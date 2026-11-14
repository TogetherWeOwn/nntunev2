"""Bounded offline ancestry traversal for the genre resolver.

Inputs are validated by the caller: parents maps each QID to its sorted,
deduplicated parent QIDs, and the traversal limits are positive integers.
"""


class Graph:
    def __init__(self, parents, barriers, max_depth, max_nodes):
        self.parents = parents
        self.barriers = barriers
        self.max_depth = max_depth
        self.max_nodes = max_nodes

    def reach(self, start):
        """Return the visited closure, whether it was capped, and cycle presence.

        The start counts toward the node budget. Barriers are neither visited
        nor expanded. Sorted parent lists make truncation deterministic.
        """
        if start in self.barriers:
            return (frozenset(), False, False)
        visited = {start}
        frontier = [start]
        depth = 0
        capped = False
        while frontier:
            nxt = []
            for node in frontier:
                for parent in self.parents.get(node, ()):
                    if parent in self.barriers or parent in visited:
                        continue
                    if depth >= self.max_depth or len(visited) >= self.max_nodes:
                        capped = True
                        continue
                    visited.add(parent)
                    nxt.append(parent)
            frontier = nxt
            depth += 1
        return (frozenset(visited), capped, self._has_cycle(start, visited))

    def _has_cycle(self, start, visited):
        state = {start: 1}  # 1 = on stack, 2 = done
        stack = [(start, iter(self.parents.get(start, ())))]
        while stack:
            node, parents = stack[-1]
            for parent in parents:
                if parent not in visited:
                    continue
                if state.get(parent) == 1:
                    return True
                if parent not in state:
                    state[parent] = 1
                    stack.append((parent, iter(self.parents.get(parent, ()))))
                    break
            else:
                state[node] = 2
                stack.pop()
        return False
