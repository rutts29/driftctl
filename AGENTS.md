# driftctl workflow

- Read `SPEC.md` and `tasks/todo.md` before changing behavior.
- Keep the project independent from the private engineering harness.
- Reuse and adapt proven private-harness code when that is faster, but remove unrelated authority and infrastructure dependencies at this repository's boundary.
- Implement one task at a time using RED → GREEN → REFACTOR.
- Do not add dependencies, harness integrations, or instruction-file mutations outside the accepted scope.
- Keep one Rust house style everywhere: rustfmt layout, explicit typed state, small named helpers, stable terminology, and behavior-first integration tests.
- Run the focused test during development and the full checks before marking a task complete.
