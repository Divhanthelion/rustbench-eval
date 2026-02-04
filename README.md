# RustBench-X Evaluation Harness

A Rust-based evaluation pipeline for benchmarking LLM code generation, based on the RustBench-X framework.

## Features

- **Multi-dimensional scoring** (RPI - Rust Proficiency Index)
  - Functional Correctness (40%)
  - Memory Safety Score (35%)
  - Idiomatic Quality (25%)

- **Tiered complexity evaluation**
  - Tier 1: Algorithmic Core (basic syntax)
  - Tier 2: Idiomatic Systems (Clippy-level)
  - Tier 3: Safety Critical (Miri verification)
  - Tier 4: Repository & Concurrency

- **Integrated toolchain**
  - `cargo check` - compilation via JSON diagnostics
  - `cargo clippy` - lint scoring via JSON diagnostics
  - `cargo test` - functional tests with timeout protection
  - `cargo miri` - undefined behavior detection (optional, with timeout)

- **Structured compiler diagnostics**
  - JSON message format for cargo check and clippy
  - Error categorization: Syntax, BorrowChecker, TypeMismatch, Lifetime, UnresolvedImport
  - "Distance to compilation" metric (error count for failed builds)
  - Unsafe code detection and flagging

- **Inference abstraction**
  - `InferenceProvider` trait for backend swapping
  - `GenerationConfig` with temperature, max_tokens, top_p, seed
  - `ModelResponse` with token counts and latency tracking
  - LM Studio implementation (OpenAI-compatible API)

- **SQLite persistence**
  - WAL mode for concurrent write performance
  - Run-level grouping (`runs` table)
  - Full evaluation history with structured diagnostics
  - Cross-model comparison queries

## Installation

```bash
cd rustbench-eval
cargo build --release
```

## Usage

### Check LM Studio Connection

```bash
./target/release/rustbench check --url http://localhost:1234/v1
```

### Generate Example Tasks

```bash
./target/release/rustbench example --output tasks.jsonl
```

### Run Evaluation

```bash
# Basic run
./target/release/rustbench run --tasks tasks.jsonl

# With Miri safety verification
./target/release/rustbench run --tasks tasks.jsonl --miri

# Save results
./target/release/rustbench run --tasks tasks.jsonl --output results.jsonl

# Custom settings
./target/release/rustbench run \
  --tasks tasks.jsonl \
  --url http://localhost:1234/v1 \
  --model local-model \
  --temperature 0.3 \
  --max-tokens 1024 \
  --timeout 30 \
  --miri \
  --output results.jsonl
```

### Run Single Task

```bash
./target/release/rustbench single --task '{"task_id":"test","tier":"algorithmic_core","prompt":"Sum a slice","signature":"fn sum(s: &[i32]) -> i32","context_code":"","dependencies":{},"tests":"#[test] fn t() { assert_eq!(sum(&[1,2,3]), 6); }","miri_compatible":true}'
```

### Query Results

```bash
# List all evaluated models
./target/release/rustbench models

# Show summary for a model
./target/release/rustbench summary --model local-model

# Compare results across models for a task
./target/release/rustbench compare --task tier1_sum_vec
```

## Task File Format (JSONL)

Each line is a JSON object:

```json
{
  "task_id": "tier1_sum_vec",
  "tier": "algorithmic_core",
  "min_rust_version": "1.75.0",
  "prompt": "Implement a function that sums all elements in a vector.",
  "signature": "pub fn sum_vec(nums: &[i32]) -> i32",
  "context_code": "",
  "dependencies": {},
  "tests": "#[test] fn test_sum() { assert_eq!(sum_vec(&[1,2,3]), 6); }",
  "miri_compatible": true,
  "canonical_solution": "pub fn sum_vec(nums: &[i32]) -> i32 { nums.iter().sum() }",
  "tags": ["iterators", "basic"]
}
```

### Tier Values

- `algorithmic_core` - Basic Rust syntax
- `idiomatic_systems` - Clippy-compliant code
- `safety_critical` - Unsafe/Miri tasks
- `repository_architecture` - Async, concurrency

## Output Format

Results are saved as JSONL with scores and diagnostics:

```json
{
  "task_id": "tier1_sum_vec",
  "tier": "algorithmic_core",
  "compiles": true,
  "compile_errors": [],
  "compiler_diagnostics": [],
  "error_count": 0,
  "tests_passed": 3,
  "tests_total": 3,
  "clippy_warnings": 0,
  "clippy_errors": 0,
  "miri_clean": true,
  "contains_unsafe": false,
  "scores": {
    "functional_correctness": 1.0,
    "memory_safety": 1.0,
    "idiomatic_quality": 1.0,
    "compilation": 1.0,
    "rpi": 1.0
  }
}
```

When compilation fails, `compiler_diagnostics` contains structured errors:

```json
{
  "compiler_diagnostics": [
    {
      "code": "E0382",
      "message": "use of moved value: `x`",
      "level": "error",
      "category": "borrow_checker"
    }
  ],
  "error_count": 1
}
```

Error categories: `syntax`, `borrow_checker`, `type_mismatch`, `lifetime`, `unresolved_import`, `other`.

## Database Schema

Results are stored in SQLite at `~/.rustbench/results.db` with WAL mode enabled.

**`runs` table** groups evaluations by execution:

| Column | Type | Description |
|--------|------|-------------|
| id | TEXT PK | Run identifier (e.g. `run_20260131_120000`) |
| model_name | TEXT | Model evaluated |
| created_at | TIMESTAMP | Run start time |
| config_json | TEXT | Generation config as JSON |

**`evaluations` table** stores per-task results:

| Column | Type | Description |
|--------|------|-------------|
| run_id | TEXT FK | References runs(id) |
| task_id | TEXT | Task identifier |
| compiles | BOOLEAN | Compilation success |
| compiler_errors_json | TEXT | Structured diagnostics as JSON |
| error_count | INTEGER | Number of compiler errors |
| tests_passed/total/failed | INTEGER | Test results |
| clippy_warnings/errors | INTEGER | Lint counts |
| miri_clean | BOOLEAN | NULL if not run, true/false if run |
| rpi_score | REAL | Composite RPI score |
| memory_safety | REAL | Memory safety sub-score |
| contains_unsafe | BOOLEAN | Whether code uses unsafe |
| generated_code | TEXT | Full LLM output |

## Requirements

- Rust toolchain (1.75+)
- LM Studio running locally (or any OpenAI-compatible endpoint)
- For Miri: `rustup +nightly component add miri`

## RPI Score Calculation

```
RPI = I_comp * (0.40 * FC + 0.35 * MSS + 0.25 * IQ)

Where:
- I_comp = 1 if compiles, 0 otherwise (RPI = 0 when compilation fails)
- FC = tests_passed / tests_total
- MSS = 1.0 (Miri clean), 0.7 (unsafe without Miri), 0.5 (tests pass, Miri fails), 0.0 (fail)
- IQ = 1.0 - (weighted_warnings / (lines_of_code * 10))
  - Error weight: 20, Warning weight: 5
```

## Evaluation Pipeline

Each task goes through these stages in order:

1. **Workspace construction** - temp Cargo project with dependencies
2. **Compilation barrier** (`cargo check --message-format=json`) - structured diagnostics, early exit on failure
3. **Static analysis** (`cargo clippy --message-format=json`) - idiomatic quality scoring
4. **Functional verification** (`cargo test`) - with configurable timeout for deadlock protection
5. **Safety verification** (`cargo +nightly miri test`) - optional, with 3x timeout, detects UB
6. **RPI calculation** - composite score from all stages

## Security Notes

Generated code runs on the host with the privileges of the current user. The evaluation uses `tempfile` for filesystem isolation (separate paths per task) but does not sandbox process execution. For untrusted models or adversarial testing, consider running the harness inside a container.

## Creating Custom Tasks

1. Write the prompt describing what to implement
2. Provide the function signature
3. Write unit tests that verify correctness
4. Tag with appropriate tier
5. Set `miri_compatible: true` for tasks involving unsafe code
6. Optionally provide canonical solution for comparison

See `tasks/example_tasks.jsonl` for templates.
