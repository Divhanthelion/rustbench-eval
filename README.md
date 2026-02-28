# RustBench-Eval

`rustbench-eval` is a high-precision Rust-based evaluation harness for benchmarking LLM code generation. Unlike generic benchmarks, it evaluates the "Rustiness" of code through multi-dimensional scoring (RPI) and uses the actual Rust toolchain (Cargo, Clippy, Miri) for verification.

## Core Stack
- **Language:** Rust 2021
- **Inference:** LM Studio (OpenAI-compatible) via `reqwest` + `InferenceProvider` trait.
- **Verification:** `cargo check`, `cargo clippy`, `cargo test`, `cargo miri`.
- **Database:** SQLite (WAL mode) via `sqlx`.

## Evaluation Pipeline

1. **Task Definition:** Reads instructions from a JSONL file.
2. **LLM Generation:** Queries a local or remote language model.
3. **Workspace Construction:** For every task, creates a unique `TempDir` containing dynamically injected dependencies and tests.
4. **Toolchain Verification:** Uses `cargo check` for syntax and borrow-checker diagnostics, followed by `cargo clippy`, `cargo test`, and optionally `cargo miri`.
5. **RPI Scoring:** Calculates a weighted composite score (0.0 to 1.0) and saves the run data to SQLite.

## Scoring System: Rust Proficiency Index (RPI)

The RPI evaluates multiple dimensions of the generated code:

- **Functional Correctness (FC)**: 40% (Tests passed vs. Total tests)
- **Memory Safety (MSS)**: 35% (Checks for Miri-clean Unsafe code)
- **Idiomatic Quality (IQ)**: 25% (Based on Clippy warnings and errors)

If code fails to compile, the RPI score is strictly **0.0**, though compilation errors are still saved as diagnostics (Distance to Compilation).

## Usage Guide

### Standard Evaluation
```bash
rustbench run --tasks tasks/rustbench_tasks.jsonl --output results.jsonl
```

### Safety-Critical (Miri) Evaluation
```bash
rustbench run --tasks tasks/rustbench_tasks.jsonl --miri --timeout 60
```

### Interactive Single Task
```bash
rustbench single --task '{"task_id":"...","prompt":"..."}' --model "gpt-4"
```

## Technical Features

- **Non-Interactive Execution:** Harness uses `--test-threads=1` and execution timeouts to prevent deadlocks from LLM-generated code.
- **Provider Abstraction:** The `InferenceProvider` trait supports Ollama, Anthropic, or OpenAI with minimal changes.
- **Diagnostic Analysis:** Automatically parses `rustc` JSON output to categorize compiler errors (Syntax, BorrowChecker, Lifetime, UnresolvedImport) to track model improvement over time.
