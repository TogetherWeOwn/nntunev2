//! Negative fixture: a dependency that declares its own global allocator.
#[global_allocator]
static CONFLICTING: std::alloc::System = std::alloc::System;
