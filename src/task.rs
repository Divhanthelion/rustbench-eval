use serde::{Deserialize, Serialize};

/// Complexity tier from RustBench-X framework
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Basic Rust syntax and logic
    AlgorithmicCore,
    /// Idiomatic Rust, Clippy-level quality
    IdiomaticSystems,
    /// Unsafe code, Miri verification
    SafetyCritical,
    /// Concurrency, async, repository context
    RepositoryArchitecture,
}

impl Tier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::AlgorithmicCore => "algorithmic_core",
            Tier::IdiomaticSystems => "idiomatic_systems",
            Tier::SafetyCritical => "safety_critical",
            Tier::RepositoryArchitecture => "repository_architecture",
        }
    }
}

/// Category of compiler error for "distance to compilation" analysis
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Syntax,
    BorrowChecker,
    TypeMismatch,
    Lifetime,
    UnresolvedImport,
    Other,
}

/// Structured compiler diagnostic extracted from JSON output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompilerDiagnostic {
    pub code: Option<String>,
    pub message: String,
    pub level: String,
    pub category: ErrorCategory,
}

impl CompilerDiagnostic {
    pub fn categorize(code: Option<&str>, message: &str) -> ErrorCategory {
        match code {
            Some("E0382" | "E0505" | "E0502" | "E0499" | "E0503" | "E0597") => {
                ErrorCategory::BorrowChecker
            }
            Some("E0106" | "E0621" | "E0495" | "E0700") => {
                ErrorCategory::Lifetime
            }
            Some("E0308" | "E0277" | "E0271" | "E0369" | "E0599") => {
                ErrorCategory::TypeMismatch
            }
            Some("E0432" | "E0433" | "E0412" | "E0425") => {
                ErrorCategory::UnresolvedImport
            }
            Some("E0063" | "E0061" | "E0054") => {
                ErrorCategory::Syntax
            }
            None if message.contains("expected") && message.contains("found") => {
                ErrorCategory::Syntax
            }
            _ => ErrorCategory::Other,
        }
    }
}

/// A single evaluation task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Unique identifier
    pub task_id: String,
    
    /// Complexity tier
    pub tier: Tier,
    
    /// Minimum Rust version required
    #[serde(default = "default_rust_version")]
    pub min_rust_version: String,
    
    /// The prompt/description for the LLM
    pub prompt: String,
    
    /// Function signature to implement
    pub signature: String,
    
    /// Any context code (structs, imports) to include
    #[serde(default)]
    pub context_code: String,
    
    /// Dependencies to add to Cargo.toml
    #[serde(default)]
    pub dependencies: std::collections::HashMap<String, String>,
    
    /// Unit tests to run
    pub tests: String,
    
    /// Should we run Miri?
    #[serde(default)]
    pub miri_compatible: bool,
    
    /// The canonical solution (for comparison/grading)
    #[serde(default)]
    pub canonical_solution: Option<String>,
    
    /// Focus areas for this task
    #[serde(default)]
    pub tags: Vec<String>,
}

fn default_rust_version() -> String {
    "1.75.0".to_string()
}

/// Result of evaluating a single task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: String,
    pub tier: Tier,

    /// Did cargo check pass?
    pub compiles: bool,
    pub compile_errors: Vec<String>,
    /// Structured compiler diagnostics with error codes and categories
    pub compiler_diagnostics: Vec<CompilerDiagnostic>,
    /// Total number of compiler errors (distance to compilation)
    pub error_count: u32,

    /// Test results
    pub tests_passed: u32,
    pub tests_total: u32,
    pub tests_failed: u32,
    pub tests_timed_out: bool,
    pub test_output: String,

    /// Clippy results
    pub clippy_warnings: u32,
    pub clippy_errors: u32,
    pub clippy_details: Vec<String>,
    pub clippy_output: String,

    /// Miri results (if applicable)
    pub miri_clean: Option<bool>,
    pub miri_errors: Vec<String>,
    pub miri_output: String,

    /// Whether the generated code contains `unsafe` blocks
    pub contains_unsafe: bool,

    /// The generated code
    pub generated_code: String,

    /// Timing info
    pub generation_time_ms: u64,
    pub compilation_time_ms: u64,

    /// Composite scores
    pub scores: Scores,
}

/// Multi-dimensional scores from RustBench-X
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Scores {
    /// Functional Correctness: tests_passed / tests_total
    pub functional_correctness: f64,
    
    /// Memory Safety Score: 1.0 if miri clean, 0.5 if tests pass but miri fails, 0.0 otherwise
    pub memory_safety: f64,
    
    /// Idiomatic Quality: 1.0 - (weighted_warnings / lines_of_code)
    pub idiomatic_quality: f64,
    
    /// Compilation Efficiency: 1.0 if compiles, 0.0 otherwise
    pub compilation: f64,
    
    /// Composite RPI score
    pub rpi: f64,
}

impl Scores {
    /// Calculate the Rust Proficiency Index
    /// RPI = I_comp * (w1*FC + w2*MSS + w3*IQ + w4*Perf)
    /// Weights: FC=0.4, MSS=0.3, IQ=0.2, Perf=0.1 (perf omitted for simplicity)
    pub fn calculate_rpi(&mut self) {
        if self.compilation < 0.5 {
            self.rpi = 0.0;
        } else {
            self.rpi = 0.4 * self.functional_correctness
                + 0.35 * self.memory_safety
                + 0.25 * self.idiomatic_quality;
        }
    }
}

impl TaskResult {
    pub fn new(task_id: String, tier: Tier) -> Self {
        Self {
            task_id,
            tier,
            compiles: false,
            compile_errors: vec![],
            compiler_diagnostics: vec![],
            error_count: 0,
            tests_passed: 0,
            tests_total: 0,
            tests_failed: 0,
            tests_timed_out: false,
            test_output: String::new(),
            clippy_warnings: 0,
            clippy_errors: 0,
            clippy_details: vec![],
            clippy_output: String::new(),
            miri_clean: None,
            miri_errors: vec![],
            miri_output: String::new(),
            contains_unsafe: false,
            generated_code: String::new(),
            generation_time_ms: 0,
            compilation_time_ms: 0,
            scores: Scores::default(),
        }
    }
}
