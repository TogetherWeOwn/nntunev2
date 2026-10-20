# Guide

Root-relative `workers/demo/src/lib.rs:4` pins the const.

Chained `run_job` and `helper_inner` (`workers/demo/src/lib.rs:7-13`)
cover both fns.

Historical pins stay: this unit is on the examined head, so even
`workers/demo/src/rotated-away.rs:1-99` never flags.
