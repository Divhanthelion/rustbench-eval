mod db;
mod evaluator;
mod lm_studio;
mod task;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use colored::*;
use std::path::PathBuf;
use std::time::Instant;
use tokio::fs;

use crate::db::Database;
use crate::evaluator::Evaluator;
use crate::lm_studio::LmStudio;
use crate::lm_studio::GenerationConfig;
use crate::task::{Task, TaskResult, Tier};

#[derive(Parser)]
#[command(name = "rustbench")]
#[command(about = "Rust code evaluation harness for LLM benchmarking")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run evaluation on a task file
    Run {
        /// Path to JSONL task file
        #[arg(short, long)]
        tasks: PathBuf,
        
        /// LM Studio URL
        #[arg(short, long, default_value = "http://localhost:1234/v1")]
        url: String,
        
        /// Model name
        #[arg(short, long, default_value = "local-model")]
        model: String,
        
        /// Temperature for generation
        #[arg(long, default_value = "0.3")]
        temperature: f32,
        
        /// Max tokens for generation
        #[arg(long, default_value = "1024")]
        max_tokens: u32,
        
        /// Run Miri for safety verification
        #[arg(long)]
        miri: bool,
        
        /// Timeout in seconds for each test run (default 30)
        #[arg(long, default_value = "30")]
        timeout: u64,
        
        /// Output file for results (JSONL)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    
    /// Check if LM Studio is available
    Check {
        #[arg(short, long, default_value = "http://localhost:1234/v1")]
        url: String,
    },
    
    /// Run a single task interactively
    Single {
        /// The task as JSON
        #[arg(short, long)]
        task: String,
        
        /// LM Studio URL
        #[arg(short, long, default_value = "http://localhost:1234/v1")]
        url: String,
        
        /// Model name
        #[arg(short, long, default_value = "local-model")]
        model: String,
    },
    
    /// Generate example task file
    Example {
        /// Output path
        #[arg(short, long, default_value = "example_tasks.jsonl")]
        output: PathBuf,
    },
    
    /// List all models in the database
    Models,
    
    /// Show summary for a model
    Summary {
        /// Model name
        #[arg(short, long)]
        model: String,
    },
    
    /// Compare results for a specific task across models
    Compare {
        /// Task ID
        #[arg(short, long)]
        task: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    
    // Initialize database
    let db_path = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".rustbench")
        .join("results.db");
    
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).await.ok();
    }
    
    match cli.command {
        Commands::Run { tasks, url, model, temperature, max_tokens, miri, timeout, output } => {
            let db = Database::new(&db_path).await.ok();
            run_evaluation(&tasks, &url, &model, temperature, max_tokens, miri, timeout, output.as_deref(), db.as_ref()).await
        }
        Commands::Check { url } => {
            check_connection(&url).await
        }
        Commands::Single { task, url, model } => {
            run_single(&task, &url, &model).await
        }
        Commands::Example { output } => {
            generate_example(&output).await
        }
        Commands::Models => {
            let db = Database::new(&db_path).await?;
            list_models(&db).await
        }
        Commands::Summary { model } => {
            let db = Database::new(&db_path).await?;
            show_summary(&db, &model).await
        }
        Commands::Compare { task } => {
            let db = Database::new(&db_path).await?;
            compare_task(&db, &task).await
        }
    }
}

async fn run_evaluation(
    tasks_path: &PathBuf,
    url: &str,
    model: &str,
    temperature: f32,
    max_tokens: u32,
    run_miri: bool,
    timeout_secs: u64,
    output: Option<&std::path::Path>,
    db: Option<&Database>,
) -> Result<()> {
    println!("{}", "╔═══════════════════════════════════════════════════════════╗".cyan());
    println!("{}", "║           RustBench-X Evaluation Pipeline                 ║".cyan());
    println!("{}", "╚═══════════════════════════════════════════════════════════╝".cyan());
    println!();
    
    // Check LM Studio
    let lm = LmStudio::new(url, model);
    if !lm.health_check().await? {
        println!("{} LM Studio not available at {}", "[ERROR]".red(), url);
        return Ok(());
    }
    println!("{} LM Studio connected", "[OK]".green());
    
    // Load tasks
    let content = fs::read_to_string(tasks_path)
        .await
        .context("Failed to read tasks file")?;
    
    let tasks: Vec<Task> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .context("Failed to parse tasks")?;
    
    println!("{} Loaded {} tasks", "[INFO]".blue(), tasks.len());
    println!();

    let evaluator = Evaluator::new(run_miri, timeout_secs);
    let mut results: Vec<TaskResult> = Vec::new();

    // Create a run record in the database
    let run_id = format!("run_{}", Utc::now().format("%Y%m%d_%H%M%S"));
    if let Some(db) = db {
        let config = GenerationConfig {
            temperature,
            max_tokens,
            ..Default::default()
        };
        let config_json = serde_json::to_string(&config).unwrap_or_default();
        if let Err(e) = db.create_run(&run_id, model, &config_json).await {
            eprintln!("  {} Failed to create run record: {}", "[WARN]".yellow(), e);
        }
    }

    let total_start = Instant::now();

    for (i, task) in tasks.iter().enumerate() {
        println!(
            "{} [{}/{}] {} ({})",
            "[TASK]".yellow(),
            i + 1,
            tasks.len(),
            task.task_id,
            tier_to_string(&task.tier)
        );
        
        // Generate code
        let gen_start = Instant::now();
        let code = match lm.generate_code(
            &task.prompt,
            &task.signature,
            &task.context_code,
            temperature,
            max_tokens,
        ).await {
            Ok(c) => c,
            Err(e) => {
                println!("  {} Generation failed: {}", "[ERROR]".red(), e);
                continue;
            }
        };
        let gen_time = gen_start.elapsed().as_millis() as u64;
        
        // Evaluate
        let mut result = evaluator.evaluate(task, &code).await?;
        result.generation_time_ms = gen_time;
        
        // Print result summary
        print_result_summary(&result);
        
        // Save to database if available
        if let Some(db) = db {
            if let Err(e) = db.save_result(&result, model, Some(&run_id)).await {
                eprintln!("  {} Failed to save to database: {}", "[WARN]".yellow(), e);
            }
        }
        
        results.push(result);
        println!();
    }
    
    // Print summary
    print_final_summary(&results, total_start.elapsed().as_secs());
    
    // Save results if output specified
    if let Some(output_path) = output {
        let json_lines: Vec<String> = results
            .iter()
            .map(|r| serde_json::to_string(r).unwrap())
            .collect();
        fs::write(output_path, json_lines.join("\n")).await?;
        println!("\n{} Results saved to {:?}", "[OK]".green(), output_path);
    }
    
    Ok(())
}

fn print_result_summary(result: &TaskResult) {
    let compile_status = if result.compiles {
        "✓".green()
    } else {
        "✗".red()
    };
    
    let test_status = if result.tests_timed_out {
        "TIMEOUT".red().to_string()
    } else if result.tests_total > 0 {
        format!("{}/{}", result.tests_passed, result.tests_total)
    } else {
        "-".to_string()
    };
    
    let clippy_status = if result.clippy_errors > 0 {
        format!("{}E {}W", result.clippy_errors, result.clippy_warnings).red()
    } else if result.clippy_warnings > 0 {
        format!("{}W", result.clippy_warnings).yellow()
    } else {
        "clean".green()
    };
    
    let miri_status = match result.miri_clean {
        Some(true) => "✓".green(),
        Some(false) => "✗".red(),
        None => "-".normal(),
    };
    
    let unsafe_flag = if result.contains_unsafe { " [unsafe]".yellow() } else { "".normal() };

    println!(
        "  Compile: {} | Tests: {} | Clippy: {} | Miri: {} | RPI: {:.2}{}",
        compile_status,
        test_status,
        clippy_status,
        miri_status,
        result.scores.rpi,
        unsafe_flag
    );

    if !result.compiles {
        if result.error_count > 0 {
            println!("  {} {} errors (distance to compilation: {})",
                "Errors:".red(), result.error_count, result.error_count);
        }
        // Show first error and categorization
        if let Some(diag) = result.compiler_diagnostics.first() {
            let code_str = diag.code.as_deref().unwrap_or("--");
            println!("  {} [{}] {:?}: {}", "First:".red(), code_str, diag.category,
                result.compile_errors.first().unwrap_or(&String::new()));
        } else if let Some(err) = result.compile_errors.first() {
            println!("  {} {}", "Error:".red(), err);
        }
    }
}

fn print_final_summary(results: &[TaskResult], duration_secs: u64) {
    println!("{}", "═══════════════════════════════════════════════════════════".cyan());
    println!("{}", "                    EVALUATION SUMMARY                      ".cyan());
    println!("{}", "═══════════════════════════════════════════════════════════".cyan());
    
    let total = results.len();
    let compiled = results.iter().filter(|r| r.compiles).count();
    let all_tests_passed = results.iter().filter(|r| r.tests_passed == r.tests_total && r.tests_total > 0 && !r.tests_timed_out).count();
    let timed_out = results.iter().filter(|r| r.tests_timed_out).count();
    let miri_clean = results.iter().filter(|r| r.miri_clean == Some(true)).count();
    
    let avg_rpi: f64 = results.iter().map(|r| r.scores.rpi).sum::<f64>() / total.max(1) as f64;
    let avg_fc: f64 = results.iter().map(|r| r.scores.functional_correctness).sum::<f64>() / total.max(1) as f64;
    let avg_ms: f64 = results.iter().map(|r| r.scores.memory_safety).sum::<f64>() / total.max(1) as f64;
    let avg_iq: f64 = results.iter().map(|r| r.scores.idiomatic_quality).sum::<f64>() / total.max(1) as f64;
    let unsafe_count = results.iter().filter(|r| r.contains_unsafe).count();
    let failed_errors: f64 = results.iter()
        .filter(|r| !r.compiles)
        .map(|r| r.error_count as f64)
        .sum::<f64>();
    let failed_count = results.iter().filter(|r| !r.compiles).count().max(1);

    println!("  Total Tasks:       {}", total);
    println!("  Compiled:          {} ({:.1}%)", compiled, 100.0 * compiled as f64 / total.max(1) as f64);
    println!("  All Tests Passed:  {} ({:.1}%)", all_tests_passed, 100.0 * all_tests_passed as f64 / total.max(1) as f64);
    if timed_out > 0 {
        println!("  Timed Out:         {} (deadlock?)", timed_out);
    }
    println!("  Miri Clean:        {}", miri_clean);
    if unsafe_count > 0 {
        println!("  Contains Unsafe:   {}", unsafe_count);
    }
    println!();
    println!("  Avg RPI Score:     {:.2}/1.00", avg_rpi);
    println!("  Avg Func. Correct: {:.2}/1.00", avg_fc);
    println!("  Avg Memory Safety: {:.2}/1.00", avg_ms);
    println!("  Avg Idiom Quality: {:.2}/1.00", avg_iq);
    if compiled < total {
        println!("  Avg Errors (fail): {:.1} (distance to compilation)", failed_errors / failed_count as f64);
    }
    println!();
    println!("  Duration:          {}s", duration_secs);
    println!("{}", "═══════════════════════════════════════════════════════════".cyan());
    
    // Per-tier breakdown
    println!("\n{}", "Score by Tier:".bold());
    for tier in &[Tier::AlgorithmicCore, Tier::IdiomaticSystems, Tier::SafetyCritical, Tier::RepositoryArchitecture] {
        let tier_results: Vec<_> = results.iter().filter(|r| &r.tier == tier).collect();
        if !tier_results.is_empty() {
            let tier_avg: f64 = tier_results.iter().map(|r| r.scores.rpi).sum::<f64>() / tier_results.len() as f64;
            println!("  {:25} {:3} tasks, avg RPI: {:.2}", tier_to_string(tier), tier_results.len(), tier_avg);
        }
    }
}

fn tier_to_string(tier: &Tier) -> String {
    match tier {
        Tier::AlgorithmicCore => "Tier1: Algorithmic".to_string(),
        Tier::IdiomaticSystems => "Tier2: Idiomatic".to_string(),
        Tier::SafetyCritical => "Tier3: Safety".to_string(),
        Tier::RepositoryArchitecture => "Tier4: Architecture".to_string(),
    }
}

async fn check_connection(url: &str) -> Result<()> {
    let lm = LmStudio::new(url, "test");
    if lm.health_check().await? {
        println!("{} LM Studio is available at {}", "[OK]".green(), url);
    } else {
        println!("{} LM Studio not available at {}", "[ERROR]".red(), url);
    }
    Ok(())
}

async fn run_single(task_json: &str, url: &str, model: &str) -> Result<()> {
    let task: Task = serde_json::from_str(task_json).context("Failed to parse task JSON")?;
    
    let lm = LmStudio::new(url, model);
    let evaluator = Evaluator::new(true, 30);
    
    println!("{} Generating code...", "[INFO]".blue());
    let code = lm.generate_code(&task.prompt, &task.signature, &task.context_code, 0.3, 1024).await?;
    
    println!("{} Generated:\n{}\n", "[CODE]".yellow(), code);
    
    println!("{} Evaluating...", "[INFO]".blue());
    let result = evaluator.evaluate(&task, &code).await?;
    
    println!("{}", serde_json::to_string_pretty(&result)?);
    
    Ok(())
}

async fn generate_example(output: &PathBuf) -> Result<()> {
    let examples = vec![
        Task {
            task_id: "tier1_sum_vec".to_string(),
            tier: Tier::AlgorithmicCore,
            min_rust_version: "1.75.0".to_string(),
            prompt: "Implement a function that sums all elements in a vector of i32.".to_string(),
            signature: "pub fn sum_vec(nums: &[i32]) -> i32".to_string(),
            context_code: String::new(),
            dependencies: Default::default(),
            tests: r#"
#[test]
fn test_sum_empty() {
    assert_eq!(sum_vec(&[]), 0);
}

#[test]
fn test_sum_positive() {
    assert_eq!(sum_vec(&[1, 2, 3, 4, 5]), 15);
}

#[test]
fn test_sum_mixed() {
    assert_eq!(sum_vec(&[-1, 1, -2, 2]), 0);
}
"#.to_string(),
            miri_compatible: true,
            canonical_solution: Some("pub fn sum_vec(nums: &[i32]) -> i32 { nums.iter().sum() }".to_string()),
            tags: vec!["iterators".to_string(), "basic".to_string()],
        },
        Task {
            task_id: "tier2_result_handling".to_string(),
            tier: Tier::IdiomaticSystems,
            min_rust_version: "1.75.0".to_string(),
            prompt: "Parse a string as i32, returning 0 if parsing fails. Use idiomatic error handling.".to_string(),
            signature: "pub fn parse_or_zero(s: &str) -> i32".to_string(),
            context_code: String::new(),
            dependencies: Default::default(),
            tests: r#"
#[test]
fn test_valid() {
    assert_eq!(parse_or_zero("42"), 42);
}

#[test]
fn test_invalid() {
    assert_eq!(parse_or_zero("hello"), 0);
}

#[test]
fn test_empty() {
    assert_eq!(parse_or_zero(""), 0);
}
"#.to_string(),
            miri_compatible: true,
            canonical_solution: Some("pub fn parse_or_zero(s: &str) -> i32 { s.parse().unwrap_or(0) }".to_string()),
            tags: vec!["error_handling".to_string(), "result".to_string()],
        },
        Task {
            task_id: "tier3_split_slice".to_string(),
            tier: Tier::SafetyCritical,
            min_rust_version: "1.75.0".to_string(),
            prompt: "Implement split_at_mut: split a mutable slice into two disjoint mutable slices at index mid. If mid > len, panic.".to_string(),
            signature: "pub fn my_split_at_mut<T>(slice: &mut [T], mid: usize) -> (&mut [T], &mut [T])".to_string(),
            context_code: String::new(),
            dependencies: Default::default(),
            tests: r#"
#[test]
fn test_split() {
    let mut v = vec![1, 2, 3, 4, 5];
    let (left, right) = my_split_at_mut(&mut v, 2);
    assert_eq!(left, &[1, 2]);
    assert_eq!(right, &[3, 4, 5]);
}

#[test]
fn test_split_empty_left() {
    let mut v = vec![1, 2, 3];
    let (left, right) = my_split_at_mut(&mut v, 0);
    assert!(left.is_empty());
    assert_eq!(right, &[1, 2, 3]);
}

#[test]
#[should_panic]
fn test_split_oob() {
    let mut v = vec![1, 2, 3];
    my_split_at_mut(&mut v, 10);
}
"#.to_string(),
            miri_compatible: true,
            canonical_solution: Some(r#"pub fn my_split_at_mut<T>(slice: &mut [T], mid: usize) -> (&mut [T], &mut [T]) {
    assert!(mid <= slice.len());
    let ptr = slice.as_mut_ptr();
    unsafe {
        (
            std::slice::from_raw_parts_mut(ptr, mid),
            std::slice::from_raw_parts_mut(ptr.add(mid), slice.len() - mid),
        )
    }
}"#.to_string()),
            tags: vec!["unsafe".to_string(), "slices".to_string(), "miri".to_string()],
        },
    ];
    
    let lines: Vec<String> = examples.iter().map(|t| serde_json::to_string(t).unwrap()).collect();
    fs::write(output, lines.join("\n")).await?;
    
    println!("{} Generated example tasks: {:?}", "[OK]".green(), output);
    println!("  - tier1_sum_vec (Algorithmic Core)");
    println!("  - tier2_result_handling (Idiomatic Systems)");
    println!("  - tier3_split_slice (Safety Critical)");
    
    Ok(())
}


async fn list_models(db: &Database) -> Result<()> {
    let models = db.list_models().await?;
    
    println!("{}", "═══════════════════════════════════════════════════════════".cyan());
    println!("{}", "                    MODELS IN DATABASE                      ".cyan());
    println!("{}", "═══════════════════════════════════════════════════════════".cyan());
    
    if models.is_empty() {
        println!("No models found in database.");
    } else {
        for model in &models {
            println!("  • {}", model);
        }
        println!("\nTotal: {} models", models.len());
    }
    
    Ok(())
}

async fn show_summary(db: &Database, model: &str) -> Result<()> {
    let summary = db.get_model_summary(model).await?;
    
    println!("{}", "═══════════════════════════════════════════════════════════".cyan());
    println!("{} {:^51} {}", "║".cyan(), format!("SUMMARY: {}", model).bold(), "║".cyan());
    println!("{}", "═══════════════════════════════════════════════════════════".cyan());
    
    println!("  Total Tasks:          {}", summary.total_tasks);
    println!("  Compile Rate:         {:.1}%", summary.compile_rate * 100.0);
    println!("  Test Pass Rate:       {:.1}%", summary.test_pass_rate * 100.0);
    println!();
    println!("  Avg RPI Score:        {:.2}/1.00", summary.avg_rpi);
    println!("  Avg Func. Correct:    {:.2}/1.00", summary.avg_functional_correctness);
    println!("  Avg Memory Safety:    {:.2}/1.00", summary.avg_memory_safety);
    println!("  Avg Idiom Quality:    {:.2}/1.00", summary.avg_idiomatic_quality);
    if summary.avg_error_count > 0.0 {
        println!("  Avg Error Count:      {:.1}", summary.avg_error_count);
    }
    println!();
    println!("  Avg Generation Time:  {:.0}ms", summary.avg_generation_time_ms);
    
    Ok(())
}

async fn compare_task(db: &Database, task_id: &str) -> Result<()> {
    let results = db.get_comparison_by_task(task_id).await?;
    
    println!("{}", "═══════════════════════════════════════════════════════════".cyan());
    println!("{} {:^51} {}", "║".cyan(), format!("COMPARISON: {}", task_id).bold(), "║".cyan());
    println!("{}", "═══════════════════════════════════════════════════════════".cyan());
    
    if results.is_empty() {
        println!("No results found for task: {}", task_id);
    } else {
        println!("\n  {:<25} {:>8} {:>8} {:>8}", "Model", "RPI", "Compiles", "Tests");
        println!("  {}", "─".repeat(60));
        
        for result in results {
            let compiles = if result.compiles { "✓".green() } else { "✗".red() };
            let tests = format!("{}/{}", result.tests_passed, result.tests_total);
            let rpi_color = if result.rpi_score >= 0.8 {
                result.rpi_score.to_string().green()
            } else if result.rpi_score >= 0.5 {
                result.rpi_score.to_string().yellow()
            } else {
                result.rpi_score.to_string().red()
            };
            
            println!("  {:<25} {:>8} {:>8} {:>8}",
                result.model,
                rpi_color,
                compiles,
                tests
            );
        }
    }
    
    Ok(())
}
