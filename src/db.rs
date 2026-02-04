use anyhow::{Context, Result};
use sqlx::{sqlite::SqlitePoolOptions, Pool, Sqlite};
use std::path::Path;

use crate::task::TaskResult;

pub struct Database {
    pool: Pool<Sqlite>,
}

impl Database {
    pub async fn new(db_path: impl AsRef<Path>) -> Result<Self> {
        let db_url = format!("sqlite:{}?mode=rwc", db_path.as_ref().display());

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect(&db_url)
            .await
            .context("Failed to connect to SQLite database")?;

        let db = Self { pool };
        db.init().await?;

        Ok(db)
    }

    async fn init(&self) -> Result<()> {
        // Enable WAL mode for better concurrent write throughput
        sqlx::query("PRAGMA journal_mode=WAL")
            .execute(&self.pool)
            .await
            .context("Failed to enable WAL mode")?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY,
                model_name TEXT NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                config_json TEXT
            )
            "#
        )
        .execute(&self.pool)
        .await
        .context("Failed to create runs table")?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS evaluations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT REFERENCES runs(id),
                task_id TEXT NOT NULL,
                tier TEXT NOT NULL,
                model TEXT NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                compiles BOOLEAN NOT NULL,
                compile_errors TEXT,
                compiler_errors_json TEXT,
                error_count INTEGER NOT NULL DEFAULT 0,
                tests_total INTEGER NOT NULL,
                tests_passed INTEGER NOT NULL,
                tests_failed INTEGER NOT NULL,
                tests_timed_out BOOLEAN NOT NULL,
                test_output TEXT,
                clippy_errors INTEGER NOT NULL,
                clippy_warnings INTEGER NOT NULL,
                clippy_output TEXT,
                miri_clean BOOLEAN,
                miri_output TEXT,
                rpi_score REAL NOT NULL,
                functional_correctness REAL NOT NULL,
                memory_safety REAL NOT NULL DEFAULT 0.0,
                idiomatic_quality REAL NOT NULL,
                compilation_time_ms INTEGER NOT NULL DEFAULT 0,
                generation_time_ms INTEGER NOT NULL,
                generated_code TEXT NOT NULL,
                contains_unsafe BOOLEAN NOT NULL DEFAULT 0
            )
            "#
        )
        .execute(&self.pool)
        .await
        .context("Failed to create evaluations table")?;

        // Add columns that may not exist in older databases
        for alter in &[
            "ALTER TABLE evaluations ADD COLUMN run_id TEXT REFERENCES runs(id)",
            "ALTER TABLE evaluations ADD COLUMN compiler_errors_json TEXT",
            "ALTER TABLE evaluations ADD COLUMN error_count INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE evaluations ADD COLUMN memory_safety REAL NOT NULL DEFAULT 0.0",
            "ALTER TABLE evaluations ADD COLUMN compilation_time_ms INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE evaluations ADD COLUMN contains_unsafe BOOLEAN NOT NULL DEFAULT 0",
        ] {
            // Ignore errors from columns that already exist
            let _ = sqlx::query(alter).execute(&self.pool).await;
        }

        // Indexes (each must be a separate statement for SQLite)
        for idx in &[
            "CREATE INDEX IF NOT EXISTS idx_evaluations_model ON evaluations(model)",
            "CREATE INDEX IF NOT EXISTS idx_evaluations_task_id ON evaluations(task_id)",
            "CREATE INDEX IF NOT EXISTS idx_evaluations_created_at ON evaluations(created_at)",
            "CREATE INDEX IF NOT EXISTS idx_evaluations_run_id ON evaluations(run_id)",
        ] {
            sqlx::query(idx)
                .execute(&self.pool)
                .await
                .context("Failed to create index")?;
        }

        Ok(())
    }
    
    pub async fn create_run(&self, run_id: &str, model: &str, config_json: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO runs (id, model_name, config_json) VALUES (?1, ?2, ?3)"
        )
        .bind(run_id)
        .bind(model)
        .bind(config_json)
        .execute(&self.pool)
        .await
        .context("Failed to create run")?;
        Ok(())
    }

    pub async fn save_result(
        &self,
        result: &TaskResult,
        model: &str,
        run_id: Option<&str>,
    ) -> Result<i64> {
        let miri_clean: Option<bool> = result.miri_clean;
        let compiler_errors_json = serde_json::to_string(&result.compiler_diagnostics)
            .unwrap_or_default();

        let id = sqlx::query(
            r#"
            INSERT INTO evaluations (
                run_id, task_id, tier, model, compiles, compile_errors,
                compiler_errors_json, error_count,
                tests_total, tests_passed, tests_failed, tests_timed_out, test_output,
                clippy_errors, clippy_warnings, clippy_output,
                miri_clean, miri_output,
                rpi_score, functional_correctness, memory_safety, idiomatic_quality,
                compilation_time_ms, generation_time_ms, generated_code, contains_unsafe
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)
            "#
        )
        .bind(run_id)
        .bind(&result.task_id)
        .bind(result.tier.as_str())
        .bind(model)
        .bind(result.compiles)
        .bind(result.compile_errors.join("\n"))
        .bind(&compiler_errors_json)
        .bind(result.error_count as i64)
        .bind(result.tests_total as i64)
        .bind(result.tests_passed as i64)
        .bind(result.tests_failed as i64)
        .bind(result.tests_timed_out)
        .bind(&result.test_output)
        .bind(result.clippy_errors as i64)
        .bind(result.clippy_warnings as i64)
        .bind(&result.clippy_output)
        .bind(miri_clean)
        .bind(&result.miri_output)
        .bind(result.scores.rpi)
        .bind(result.scores.functional_correctness)
        .bind(result.scores.memory_safety)
        .bind(result.scores.idiomatic_quality)
        .bind(result.compilation_time_ms as i64)
        .bind(result.generation_time_ms as i64)
        .bind(&result.generated_code)
        .bind(result.contains_unsafe)
        .execute(&self.pool)
        .await
        .context("Failed to insert evaluation result")?
        .last_insert_rowid();

        Ok(id)
    }
    
    pub async fn get_results_by_model(&self, model: &str) -> Result<Vec<DbEvaluation>> {
        let results = sqlx::query_as::<_, DbEvaluation>(
            r#"SELECT * FROM evaluations WHERE model = ?1 ORDER BY created_at DESC"#
        )
        .bind(model)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch results by model")?;
        
        Ok(results)
    }
    
    pub async fn get_comparison_by_task(&self, task_id: &str) -> Result<Vec<DbEvaluation>> {
        let results = sqlx::query_as::<_, DbEvaluation>(
            r#"SELECT * FROM evaluations WHERE task_id = ?1 ORDER BY rpi_score DESC"#
        )
        .bind(task_id)
        .fetch_all(&self.pool)
        .await
        .context("Failed to fetch comparison by task")?;
        
        Ok(results)
    }
    
    pub async fn get_model_summary(&self, model: &str) -> Result<ModelSummary> {
        let summary = sqlx::query_as::<_, ModelSummary>(
            r#"
            SELECT
                model,
                COUNT(*) as total_tasks,
                AVG(CASE WHEN compiles THEN 1.0 ELSE 0.0 END) as compile_rate,
                AVG(CASE WHEN tests_passed = tests_total AND tests_total > 0 THEN 1.0 ELSE 0.0 END) as test_pass_rate,
                AVG(rpi_score) as avg_rpi,
                AVG(functional_correctness) as avg_functional_correctness,
                AVG(memory_safety) as avg_memory_safety,
                AVG(idiomatic_quality) as avg_idiomatic_quality,
                AVG(generation_time_ms) as avg_generation_time_ms,
                AVG(error_count) as avg_error_count
            FROM evaluations
            WHERE model = ?1
            GROUP BY model
            "#
        )
        .bind(model)
        .fetch_one(&self.pool)
        .await
        .context("Failed to fetch model summary")?;

        Ok(summary)
    }
    
    pub async fn list_models(&self) -> Result<Vec<String>> {
        let models: Vec<(String,)> = sqlx::query_as(
            r#"SELECT DISTINCT model FROM evaluations ORDER BY model"#
        )
        .fetch_all(&self.pool)
        .await
        .context("Failed to list models")?;
        
        Ok(models.into_iter().map(|(m,)| m).collect())
    }
}

#[derive(sqlx::FromRow, Debug)]
pub struct DbEvaluation {
    pub id: i64,
    pub run_id: Option<String>,
    pub task_id: String,
    pub tier: String,
    pub model: String,
    pub created_at: String,
    pub compiles: bool,
    pub compile_errors: Option<String>,
    pub compiler_errors_json: Option<String>,
    pub error_count: i64,
    pub tests_total: i64,
    pub tests_passed: i64,
    pub tests_failed: i64,
    pub tests_timed_out: bool,
    pub test_output: Option<String>,
    pub clippy_errors: i64,
    pub clippy_warnings: i64,
    pub clippy_output: Option<String>,
    pub miri_clean: Option<bool>,
    pub miri_output: Option<String>,
    pub rpi_score: f64,
    pub functional_correctness: f64,
    pub memory_safety: f64,
    pub idiomatic_quality: f64,
    pub compilation_time_ms: i64,
    pub generation_time_ms: i64,
    pub generated_code: String,
    pub contains_unsafe: bool,
}

#[derive(sqlx::FromRow, Debug)]
pub struct ModelSummary {
    pub model: String,
    pub total_tasks: i64,
    pub compile_rate: f64,
    pub test_pass_rate: f64,
    pub avg_rpi: f64,
    pub avg_functional_correctness: f64,
    pub avg_memory_safety: f64,
    pub avg_idiomatic_quality: f64,
    pub avg_generation_time_ms: f64,
    pub avg_error_count: f64,
}
