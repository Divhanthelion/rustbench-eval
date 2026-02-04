use crate::task::{CompilerDiagnostic, ErrorCategory, Task, TaskResult};
use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;
use std::path::Path;
use std::process::Stdio;
use std::time::Instant;
use tempfile::TempDir;
use tokio::fs;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

/// The evaluation pipeline
pub struct Evaluator {
    /// Run Miri for safety-critical tasks
    run_miri: bool,
    /// Timeout for compilation/tests (seconds)
    timeout_secs: u64,
}

/// Structured JSON message from `cargo --message-format=json`
#[derive(Deserialize)]
struct CargoMessage {
    reason: Option<String>,
    message: Option<CargoCompilerMessage>,
}

#[derive(Deserialize)]
struct CargoCompilerMessage {
    code: Option<CargoErrorCode>,
    level: Option<String>,
    message: Option<String>,
    rendered: Option<String>,
}

#[derive(Deserialize)]
struct CargoErrorCode {
    code: Option<String>,
}

impl Evaluator {
    pub fn new(run_miri: bool, timeout_secs: u64) -> Self {
        Self { run_miri, timeout_secs }
    }

    /// Evaluate a single task with generated code
    pub async fn evaluate(&self, task: &Task, generated_code: &str) -> Result<TaskResult> {
        let mut result = TaskResult::new(task.task_id.clone(), task.tier.clone());
        result.generated_code = generated_code.to_string();
        result.contains_unsafe = generated_code.contains("unsafe ");

        // Create temporary Cargo project
        let temp_dir = TempDir::new().context("Failed to create temp directory")?;
        let project_path = temp_dir.path();

        self.setup_cargo_project(project_path, task, generated_code).await?;

        // Step 1: cargo check (JSON output for structured diagnostics)
        let start = Instant::now();
        let check_result = self.run_cargo_check(project_path).await?;
        result.compilation_time_ms = start.elapsed().as_millis() as u64;

        result.compiles = check_result.success;
        result.compile_errors = check_result.errors;
        result.compiler_diagnostics = check_result.diagnostics;
        result.error_count = result.compiler_diagnostics.iter()
            .filter(|d| d.level == "error")
            .count() as u32;

        if !result.compiles {
            result.scores.compilation = 0.0;
            result.scores.calculate_rpi();
            return Ok(result);
        }
        result.scores.compilation = 1.0;

        // Step 2: cargo clippy (JSON output for structured lint info)
        let clippy_result = self.run_cargo_clippy(project_path).await?;
        result.clippy_warnings = clippy_result.warnings;
        result.clippy_errors = clippy_result.errors;
        result.clippy_details = clippy_result.details.clone();
        result.clippy_output = clippy_result.output;

        // Calculate idiomatic quality score
        let loc = generated_code.lines().count().max(1) as f64;
        let weighted_warnings = (result.clippy_errors * 20 + result.clippy_warnings * 5) as f64;
        result.scores.idiomatic_quality = (1.0 - weighted_warnings / (loc * 10.0)).clamp(0.0, 1.0);

        // Step 3: cargo test
        let test_result = self.run_cargo_test(project_path).await?;
        result.tests_passed = test_result.passed;
        result.tests_total = test_result.total;
        result.tests_failed = test_result.total - test_result.passed;
        result.tests_timed_out = test_result.timed_out;
        result.test_output = test_result.output;

        result.scores.functional_correctness = if test_result.timed_out {
            0.0
        } else if test_result.total > 0 {
            test_result.passed as f64 / test_result.total as f64
        } else {
            0.0
        };

        // Step 4: cargo miri (if applicable)
        if self.run_miri && task.miri_compatible {
            let miri_result = self.run_cargo_miri(project_path).await?;
            result.miri_clean = miri_result.clean;
            result.miri_errors = miri_result.errors.clone();
            result.miri_output = miri_result.output;

            result.scores.memory_safety = match miri_result.clean {
                Some(true) => 1.0,
                Some(false) if result.tests_passed > 0 => 0.5,
                Some(false) => 0.0,
                None => result.scores.functional_correctness, // Miri unavailable
            };
        } else if result.contains_unsafe && !task.miri_compatible {
            // Unsafe code present but Miri not applicable — flag but don't penalize fully
            result.scores.memory_safety = if result.tests_passed > 0 { 0.7 } else { 0.0 };
        } else {
            // No Miri, no unsafe — trust test results
            result.scores.memory_safety = result.scores.functional_correctness;
        }

        result.scores.calculate_rpi();

        Ok(result)
    }

    /// Create the Cargo project structure
    async fn setup_cargo_project(&self, path: &Path, task: &Task, code: &str) -> Result<()> {
        let src_dir = path.join("src");
        fs::create_dir_all(&src_dir).await?;

        let mut cargo_toml = r#"[package]
name = "eval_task"
version = "0.1.0"
edition = "2021"

[dependencies]
"#.to_string();

        for (name, version) in &task.dependencies {
            cargo_toml.push_str(&format!("{} = \"{}\"\n", name, version));
        }

        fs::write(path.join("Cargo.toml"), cargo_toml).await?;

        // Check if code looks like a complete item or just a body
        let code_trimmed = code.trim();
        let final_code = if looks_like_rust_item(code_trimmed) {
            code.to_string()
        } else {
            format!("{} {{\n{}\n}}", task.signature, code_trimmed)
        };

        let lib_rs = format!(
            r#"#![allow(dead_code)]
#![allow(unused_imports)]

{}

{}

#[cfg(test)]
mod tests {{
    use super::*;

{}
}}
"#,
            task.context_code,
            final_code,
            indent_code(&task.tests, 4)
        );

        fs::write(src_dir.join("lib.rs"), lib_rs).await?;

        Ok(())
    }

    /// Run cargo check with JSON message format for structured diagnostics
    async fn run_cargo_check(&self, path: &Path) -> Result<CheckResult> {
        let output = Command::new("cargo")
            .arg("check")
            .arg("--message-format=json")
            .current_dir(path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to run cargo check")?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut diagnostics = Vec::new();
        let mut errors = Vec::new();

        // Parse JSON lines from stdout
        for line in stdout.lines() {
            if let Ok(msg) = serde_json::from_str::<CargoMessage>(line) {
                if msg.reason.as_deref() == Some("compiler-message") {
                    if let Some(compiler_msg) = msg.message {
                        let level = compiler_msg.level.as_deref().unwrap_or("unknown");
                        if level == "error" {
                            let message = compiler_msg.message.as_deref().unwrap_or("unknown error");
                            let code_str = compiler_msg.code
                                .as_ref()
                                .and_then(|c| c.code.as_deref());

                            let category = CompilerDiagnostic::categorize(code_str, message);

                            diagnostics.push(CompilerDiagnostic {
                                code: code_str.map(String::from),
                                message: message.to_string(),
                                level: level.to_string(),
                                category,
                            });

                            if let Some(rendered) = compiler_msg.rendered {
                                errors.push(rendered.lines().next().unwrap_or("").to_string());
                            } else {
                                errors.push(message.to_string());
                            }
                        }
                    }
                }
            }
        }

        // Fallback: if JSON parsing yielded nothing but exit code failed, parse stderr
        if !output.status.success() && diagnostics.is_empty() {
            for line in stderr.lines() {
                if line.contains("error") {
                    errors.push(line.to_string());
                    diagnostics.push(CompilerDiagnostic {
                        code: None,
                        message: line.to_string(),
                        level: "error".to_string(),
                        category: ErrorCategory::Other,
                    });
                }
            }
        }

        Ok(CheckResult {
            success: output.status.success(),
            errors,
            diagnostics,
        })
    }

    /// Run cargo clippy with JSON message format for structured lint info
    async fn run_cargo_clippy(&self, path: &Path) -> Result<ClippyResult> {
        let output = Command::new("cargo")
            .arg("clippy")
            .arg("--message-format=json")
            .arg("--")
            .arg("-W")
            .arg("clippy::all")
            .current_dir(path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("Failed to run cargo clippy")?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        let mut warnings: u32 = 0;
        let mut errors: u32 = 0;
        let mut details = Vec::new();

        for line in stdout.lines() {
            if let Ok(msg) = serde_json::from_str::<CargoMessage>(line) {
                if msg.reason.as_deref() == Some("compiler-message") {
                    if let Some(compiler_msg) = msg.message {
                        match compiler_msg.level.as_deref() {
                            Some("warning") => {
                                warnings += 1;
                                if details.len() < 10 {
                                    if let Some(m) = compiler_msg.message {
                                        details.push(m);
                                    }
                                }
                            }
                            Some("error") => {
                                errors += 1;
                                if details.len() < 10 {
                                    if let Some(m) = compiler_msg.message {
                                        details.push(m);
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // Fallback: if JSON yielded nothing, count from stderr
        if warnings == 0 && errors == 0 {
            warnings = stderr.matches("warning:").count() as u32;
            errors = stderr.matches("error:").count() as u32;
            if details.is_empty() {
                details = stderr.lines()
                    .filter(|l| l.contains("warning:") || l.contains("error:"))
                    .take(10)
                    .map(|s| s.to_string())
                    .collect();
            }
        }

        Ok(ClippyResult { warnings, errors, details, output: stderr.to_string() })
    }

    /// Run cargo test with timeout to prevent deadlocks
    async fn run_cargo_test(&self, path: &Path) -> Result<TestResult> {
        let test_future = Command::new("cargo")
            .arg("test")
            .arg("--")
            .arg("--test-threads=1")
            .current_dir(path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();

        let output = match timeout(Duration::from_secs(self.timeout_secs), test_future).await {
            Ok(result) => result.context("Failed to run cargo test")?,
            Err(_) => {
                return Ok(TestResult {
                    passed: 0,
                    total: 1,
                    timed_out: true,
                    output: "TIMEOUT: test execution exceeded time limit".to_string(),
                });
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Primary signal: parse the "X passed; Y failed" summary line
        let re = Regex::new(r"(\d+) passed.*?(\d+) failed").unwrap();

        if let Some(caps) = re.captures(&stdout) {
            let passed: u32 = caps.get(1).map(|m| m.as_str().parse().unwrap_or(0)).unwrap_or(0);
            let failed: u32 = caps.get(2).map(|m| m.as_str().parse().unwrap_or(0)).unwrap_or(0);
            Ok(TestResult {
                passed,
                total: passed + failed,
                timed_out: false,
                output: stdout.to_string(),
            })
        } else if stdout.contains("running 0 tests") {
            Ok(TestResult { passed: 0, total: 0, timed_out: false, output: stdout.to_string() })
        } else {
            // Fallback: use exit code as primary signal when regex can't parse
            let (passed, total) = if output.status.success() { (1, 1) } else { (0, 1) };
            Ok(TestResult { passed, total, timed_out: false, output: stdout.to_string() })
        }
    }

    /// Run cargo miri with timeout protection
    async fn run_cargo_miri(&self, path: &Path) -> Result<MiriResult> {
        // Miri is significantly slower — use 3x the normal timeout
        let miri_timeout = Duration::from_secs(self.timeout_secs * 3);

        let miri_future = Command::new("cargo")
            .arg("+nightly")
            .arg("miri")
            .arg("test")
            .env("MIRIFLAGS", "-Zmiri-disable-isolation")
            .current_dir(path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();

        let output = match timeout(miri_timeout, miri_future).await {
            Ok(Ok(out)) => out,
            Ok(Err(_)) => {
                // Miri not available (e.g. not installed)
                return Ok(MiriResult {
                    clean: None,
                    errors: vec!["Miri not available".to_string()],
                    output: "Miri not available on this toolchain".to_string(),
                });
            }
            Err(_) => {
                // Timeout
                return Ok(MiriResult {
                    clean: Some(false),
                    errors: vec!["Miri timed out".to_string()],
                    output: "TIMEOUT: Miri execution exceeded time limit".to_string(),
                });
            }
        };

        let stderr = String::from_utf8_lossy(&output.stderr);

        // Check if Miri itself is not installed
        if stderr.contains("can't find crate") || stderr.contains("no such subcommand") {
            return Ok(MiriResult {
                clean: None,
                errors: vec!["Miri not installed".to_string()],
                output: stderr.to_string(),
            });
        }

        let errors: Vec<String> = stderr
            .lines()
            .filter(|l| l.contains("Undefined Behavior") || l.contains("error:"))
            .map(|s| s.to_string())
            .collect();

        Ok(MiriResult {
            clean: Some(output.status.success() && errors.is_empty()),
            errors,
            output: stderr.to_string(),
        })
    }
}

struct CheckResult {
    success: bool,
    errors: Vec<String>,
    diagnostics: Vec<CompilerDiagnostic>,
}

struct ClippyResult {
    warnings: u32,
    errors: u32,
    details: Vec<String>,
    output: String,
}

struct TestResult {
    passed: u32,
    total: u32,
    timed_out: bool,
    output: String,
}

struct MiriResult {
    clean: Option<bool>,
    errors: Vec<String>,
    output: String,
}

/// Indent code by a number of spaces
fn indent_code(code: &str, spaces: usize) -> String {
    let indent = " ".repeat(spaces);
    code.lines()
        .map(|l| format!("{}{}", indent, l))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Check if code looks like a valid Rust item (not just a function body)
fn looks_like_rust_item(code: &str) -> bool {
    let code = code.trim();
    code.starts_with("pub fn ")
        || code.starts_with("fn ")
        || code.starts_with("impl ")
        || code.starts_with("pub struct ")
        || code.starts_with("struct ")
        || code.starts_with("pub enum ")
        || code.starts_with("enum ")
        || code.starts_with("pub type ")
        || code.starts_with("type ")
        || code.starts_with("pub const ")
        || code.starts_with("const ")
        || code.starts_with("pub static ")
        || code.starts_with("static ")
        || code.starts_with("use ")
        || code.starts_with("mod ")
        || code.starts_with("#[")
}
