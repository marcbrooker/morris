//! Morris - AI-Powered Mutation Testing for Rust
//!
//! A fixed-workflow mutation testing tool that uses AWS Bedrock (Claude) to
//! intelligently select and analyze mutations, while handling file discovery,
//! test execution, and mutation application deterministically.

use aws_sdk_bedrockruntime::{
    operation::converse::ConverseOutput,
    types::{ContentBlock, ConversationRole, Message, SystemContentBlock},
};
use clap::Parser;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{debug, info};
use tracing_subscriber::EnvFilter;

/// A single mutation proposed by the AI.
#[derive(Debug, Deserialize)]
struct Mutation {
    file_path: String,
    line_number: usize,
    original_line: String,
    mutated_line: String,
    description: String,
}

/// The AI's response containing proposed mutations.
#[derive(Debug, Deserialize)]
struct MutationPlan {
    mutations: Vec<Mutation>,
}

/// Result of testing a single mutation.
#[derive(Debug)]
struct MutationResult {
    mutation: Mutation,
    outcome: MutationOutcome,
}

/// Possible outcomes of a mutation test.
#[derive(Debug)]
enum MutationOutcome {
    Survived,
    Killed,
    Timeout,
    BuildError(String),
    LineMismatch(String),
}

impl std::fmt::Display for MutationOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Survived => write!(f, "SURVIVED"),
            Self::Killed => write!(f, "KILLED"),
            Self::Timeout => write!(f, "TIMEOUT"),
            Self::BuildError(e) => write!(f, "BUILD ERROR: {e}"),
            Self::LineMismatch(e) => write!(f, "LINE MISMATCH: {e}"),
        }
    }
}

/// Configuration parsed from command-line arguments.
#[derive(Debug, Parser)]
#[command(name = "cargo", bin_name = "cargo")]
enum CargoCli {
    /// AI-powered mutation testing for Rust
    Morris(Config),
}

/// AI-powered mutation testing for Rust.
#[derive(Debug, Default, Parser)]
struct Config {
    /// Automatically apply test improvements
    #[arg(long = "auto")]
    auto_mode: bool,
    /// Use Claude Haiku for faster, less thorough analysis
    #[arg(long = "quick")]
    quick_mode: bool,
    /// Enable debug logging
    #[arg(short, long)]
    verbose: bool,
    /// Source files or directories to test (default: all of src/)
    #[arg()]
    paths: Vec<PathBuf>,
}

impl Config {
    fn model_id(&self) -> &str {
        if self.quick_mode {
            "us.anthropic.claude-haiku-4-5-20251001-v1:0"
        } else {
            "us.anthropic.claude-sonnet-4-6"
        }
    }
}

/// Discover all `.rs` files under `src/` recursively.
fn list_source_files(base: &Path) -> Vec<PathBuf> {
    let src = base.join("src");
    if !src.exists() {
        return Vec::new();
    }
    let mut files = Vec::new();
    collect_rs_files(&src, &mut files);
    files.sort();
    files
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Resolve user-provided paths into a sorted list of `.rs` files.
fn filter_source_files(
    cwd: &Path,
    paths: &[PathBuf],
) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut files = Vec::new();
    for p in paths {
        let abs = if p.is_absolute() {
            p.clone()
        } else {
            cwd.join(p)
        };
        let abs = abs
            .canonicalize()
            .map_err(|e| format!("{}: {e}", p.display()))?;
        if abs.is_dir() {
            collect_rs_files(&abs, &mut files);
        } else if abs.extension().and_then(|s| s.to_str()) == Some("rs") {
            files.push(abs);
        } else {
            return Err(format!("{}: not a .rs file or directory", p.display()).into());
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// Run `cargo test --quiet` and return (success, duration, output).
async fn run_cargo_test(timeout: Duration) -> (bool, Duration, String) {
    let start = Instant::now();
    let result = tokio::time::timeout(
        timeout,
        tokio::process::Command::new("cargo")
            .args(["test", "--quiet"])
            .output(),
    )
    .await;
    let elapsed = start.elapsed();

    match result {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let combined = format!("{stdout}{stderr}");
            (output.status.success(), elapsed, combined)
        }
        Ok(Err(e)) => (false, elapsed, format!("Failed to run cargo test: {e}")),
        Err(_) => (false, elapsed, "TIMEOUT".to_string()),
    }
}

/// Apply a single-line mutation, test, and restore. Returns the outcome.
async fn test_line_mutation(
    file_path: &str,
    line_number: usize,
    original_line: &str,
    mutated_line: &str,
    timeout: Duration,
) -> MutationOutcome {
    let backup_path = format!("{file_path}.morris-backup");

    // Read and backup
    let Ok(original) = std::fs::read_to_string(file_path) else {
        return MutationOutcome::BuildError("Cannot read file".to_string());
    };
    if std::fs::write(&backup_path, &original).is_err() {
        return MutationOutcome::BuildError("Cannot create backup".to_string());
    }

    let lines: Vec<&str> = original.lines().collect();

    // Find the correct line (exact or fuzzy ±5)
    let Some(target) = find_target_line(&lines, line_number, original_line) else {
        let actual = if line_number > 0 && line_number <= lines.len() {
            lines[line_number - 1]
        } else {
            "<out of range>"
        };
        debug!(
            "Line mismatch at {file_path}:{line_number}\n  expected: '{}'\n  actual:   '{actual}'",
            original_line.trim()
        );
        let _ = std::fs::remove_file(&backup_path);
        return MutationOutcome::LineMismatch(format!(
            "line {line_number}: expected '{}', found '{}'",
            original_line.trim(),
            actual.trim()
        ));
    };

    debug!(
        "Matched line {target} in {file_path}: '{}'",
        lines[target - 1].trim()
    );

    // Apply mutation
    let mut new_lines: Vec<&str> = lines.clone();
    new_lines[target - 1] = mutated_line;
    let mutated_content = new_lines.join("\n");

    if std::fs::write(file_path, &mutated_content).is_err() {
        let _ = std::fs::copy(&backup_path, file_path);
        let _ = std::fs::remove_file(&backup_path);
        return MutationOutcome::BuildError("Cannot write mutation".to_string());
    }

    // Test
    let (success, _, output) = run_cargo_test(timeout).await;

    // Restore
    let _ = std::fs::copy(&backup_path, file_path);
    let _ = std::fs::remove_file(&backup_path);

    if output == "TIMEOUT" {
        MutationOutcome::Timeout
    } else if success {
        MutationOutcome::Survived
    } else if output.contains("error[E") || output.contains("could not compile") {
        debug!("Build error for mutation at {file_path}:{target}:\n{output}");
        MutationOutcome::BuildError("compilation failed".to_string())
    } else {
        MutationOutcome::Killed
    }
}

/// Find the target line index (1-based), with fuzzy search ±5 lines.
fn find_target_line(lines: &[&str], line_number: usize, expected: &str) -> Option<usize> {
    if line_number == 0 || line_number > lines.len() {
        return None;
    }

    let normalize = |s: &str| s.trim().replace("\\\"", "\"").replace("\\'", "'");
    let expected_norm = normalize(expected);

    // Exact match
    if normalize(lines[line_number - 1]) == expected_norm {
        return Some(line_number);
    }

    // Fuzzy search ±10
    let start = line_number.saturating_sub(10).max(1);
    let end = (line_number + 10).min(lines.len());
    for i in start..=end {
        if normalize(lines[i - 1]) == expected_norm {
            info!("Line content found at {i} instead of {line_number}");
            return Some(i);
        }
    }
    None
}

/// Strip markdown code fences from a response if present.
fn strip_code_fences(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```") {
        // Skip the language tag on the first line
        let rest = rest.split_once('\n').map_or(rest, |(_, r)| r);
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        trimmed
    }
}

/// Call Bedrock Converse API and extract the text response.
async fn converse(
    client: &aws_sdk_bedrockruntime::Client,
    model_id: &str,
    system: &str,
    user_message: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let resp: ConverseOutput = client
        .converse()
        .model_id(model_id)
        .system(SystemContentBlock::Text(system.to_string()))
        .messages(
            Message::builder()
                .role(ConversationRole::User)
                .content(ContentBlock::Text(user_message.to_string()))
                .build()
                .map_err(|e| format!("Failed to build message: {e}"))?,
        )
        .send()
        .await?;

    let output = resp.output().ok_or("No output in response")?;
    let message = output.as_message().map_err(|_| "Output is not a message")?;
    for block in message.content() {
        if let ContentBlock::Text(text) = block {
            return Ok(text.clone());
        }
    }
    Err("No text in response".into())
}

fn init_logging(verbose: bool) {
    let level = if verbose { "debug" } else { "warn" };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::from_default_env().add_directive(format!("morris={level}").parse().unwrap()),
        )
        .init();
}

/// Read all source files into a single string for the AI prompt.
fn read_all_sources(
    cwd: &Path,
    source_files: &[PathBuf],
) -> Result<String, Box<dyn std::error::Error>> {
    use std::fmt::Write;
    let mut contents = String::new();
    for path in source_files {
        let relative = path.strip_prefix(cwd).unwrap_or(path);
        let raw = std::fs::read_to_string(path)?;
        writeln!(contents, "=== {} ===", relative.display())?;
        for (i, line) in raw.lines().enumerate() {
            writeln!(contents, "{:>4}| {line}", i + 1)?;
        }
        contents.push('\n');
    }
    Ok(contents)
}

/// Build the prompt asking the AI for a mutation plan.
fn build_mutation_prompt(file_contents: &str) -> String {
    format!(
        "Analyze this Rust project and propose 5-8 strategic single-line mutations that are \
         likely to survive the existing test suite (i.e., reveal test coverage gaps).\n\n\
         Focus on:\n\
         - Boundary conditions (>, <, >=, <=)\n\
         - Arithmetic operators (+, -, *, /)\n\
         - Logic operators (&&, ||, !, ==, !=)\n\
         - Off-by-one errors\n\
         - Return value changes\n\n\
         Respond with ONLY a JSON object (no markdown fences) in this exact format:\n\
         {{\"mutations\": [\n\
           {{\"file_path\": \"src/lib.rs\", \"line_number\": 42, \
             \"original_line\": \"    if x > 0 {{\", \
             \"mutated_line\": \"    if x >= 0 {{\", \
             \"description\": \"Change > to >= to test boundary\"}}\n\
         ]}}\n\n\
         IMPORTANT:\n\
         - Use paths relative to the project root\n\
         - Line numbers are shown as \"  N| code\" — use the number before the pipe\n\
         - Copy original_line EXACTLY as it appears AFTER the \"| \" prefix (including indentation)\n\
         - Each mutation must be a single line change that still compiles\n\
         - The mutated_line must have the same indentation as original_line\n\n\
         Source files (with line numbers):\n{file_contents}"
    )
}

/// Run all mutations and collect results.
async fn run_mutations(
    cwd: &Path,
    mutations: Vec<Mutation>,
    timeout: Duration,
) -> Vec<MutationResult> {
    let mut results = Vec::new();
    for (i, mutation) in mutations.into_iter().enumerate() {
        let full_path = cwd.join(&mutation.file_path);
        let file_path_str = full_path.to_str().unwrap_or(&mutation.file_path);

        eprint!(
            "   [{}/{}] {}:{} - {}... ",
            i + 1,
            results.len() + 1,
            mutation.file_path,
            mutation.line_number,
            mutation.description
        );

        let outcome = test_line_mutation(
            file_path_str,
            mutation.line_number,
            &mutation.original_line,
            &mutation.mutated_line,
            timeout,
        )
        .await;

        let icon = match &outcome {
            MutationOutcome::Survived => "❌ SURVIVED",
            MutationOutcome::Killed => "✅ KILLED",
            MutationOutcome::Timeout => "⏱️  TIMEOUT",
            MutationOutcome::BuildError(_) => "🔧 BUILD ERROR",
            MutationOutcome::LineMismatch(_) => "⚠️  LINE MISMATCH",
        };
        eprintln!("{icon}");

        results.push(MutationResult { mutation, outcome });
    }
    results
}

/// Format mutation results into a summary string.
fn format_results_summary(results: &[MutationResult]) -> String {
    use std::fmt::Write;
    let mut summary = String::new();
    for r in results {
        let _ = writeln!(
            summary,
            "- {}:{} [{}] {} | {} → {}",
            r.mutation.file_path,
            r.mutation.line_number,
            r.outcome,
            r.mutation.description,
            r.mutation.original_line.trim(),
            r.mutation.mutated_line.trim(),
        );
    }
    summary
}

/// Build the analysis prompt based on mode.
fn build_analysis_prompt(auto_mode: bool, results_summary: &str, file_contents: &str) -> String {
    if auto_mode {
        format!(
            "Results:\n{results_summary}\n\n\
             Source code:\n{file_contents}\n\n\
             Write new #[test] functions that catch each SURVIVED mutation.\n\
             Output ONLY the new test functions, nothing else. No explanations, no module wrapper, no use statements.\n\
             Wrap them in a single fenced code block:\n\
             ```rust\n\
             #[test]\n\
             fn test_name() {{ ... }}\n\
             ```"
        )
    } else {
        format!(
            "These mutations were tested against the project's test suite.\n\n\
             Results:\n{results_summary}\n\n\
             Source code:\n{file_contents}\n\n\
             For each SURVIVED mutation, explain:\n\
             1. Why the current tests don't catch it\n\
             2. A specific test that would catch it (show the code)\n\n\
             Be concise and actionable."
        )
    }
}

/// Apply file changes from the AI analysis output.
///
/// Extracts new test functions from the AI response and inserts them
/// into the existing test module of each source file.
async fn auto_apply(
    cwd: &Path,
    analysis: &str,
    test_timeout: Duration,
) -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("\n🔧 Auto-applying test improvements...");

    // Extract code from the first ```rust block (or bare ``` block)
    let new_tests = extract_code_block(analysis);
    if new_tests.is_empty() {
        eprintln!("   ⚠️  No code block found in AI response");
        return Ok(());
    }

    // Find source files with a test module and inject the new tests
    let mut applied = 0;
    for path in list_source_files(cwd) {
        let source = std::fs::read_to_string(&path)?;
        if source.contains("#[cfg(test)]")
            && let Some(pos) = source.rfind("\n}")
        {
            let mut patched = String::with_capacity(source.len() + new_tests.len() + 2);
            patched.push_str(&source[..pos]);
            patched.push('\n');
            patched.push_str(&new_tests);
            patched.push_str(&source[pos..]);
            let rel = path.strip_prefix(cwd).unwrap_or(&path);
            eprintln!("   Writing {}...", rel.display());
            std::fs::write(&path, patched)?;
            applied += 1;
        }
    }

    if applied == 0 {
        eprintln!("   ⚠️  Could not find test module to patch");
        return Ok(());
    }

    eprintln!("   Verifying tests...");
    let (ok, _, output) = run_cargo_test(test_timeout).await;
    if ok {
        eprintln!("   ✅ All tests pass with improvements!");
    } else {
        eprintln!("   ❌ Tests failed after auto-apply. Check output:\n{output}");
    }
    Ok(())
}

/// Extract the contents of all fenced code blocks from text.
fn extract_code_block(text: &str) -> String {
    let mut in_block = false;
    let mut code = String::new();
    for line in text.lines() {
        if !in_block {
            if line.trim().starts_with("```rust") || line.trim() == "```" {
                in_block = true;
            }
        } else if line.trim().starts_with("```") {
            in_block = false;
        } else {
            if !code.is_empty() {
                code.push('\n');
            }
            code.push_str(line);
        }
    }
    code
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let CargoCli::Morris(config) = CargoCli::parse();
    init_logging(config.verbose);

    eprintln!(
        "🧬 Morris v{} - AI-Powered Mutation Testing\n",
        env!("CARGO_PKG_VERSION")
    );

    let cwd = std::env::current_dir()?;
    let model_id = config.model_id();

    // Step 1: Discover source files
    eprintln!("📁 Discovering source files...");
    let source_files = if config.paths.is_empty() {
        list_source_files(&cwd)
    } else {
        filter_source_files(&cwd, &config.paths)?
    };
    if source_files.is_empty() {
        eprintln!("❌ No Rust source files found");
        return Ok(());
    }
    for f in &source_files {
        eprintln!("   {}", f.display());
    }

    // Step 2: Read all source files
    eprintln!("\n📖 Reading source files...");
    let file_contents = read_all_sources(&cwd, &source_files)?;

    // Step 3: Run baseline tests
    eprintln!("⏱️  Running baseline tests...");
    let (baseline_ok, baseline_duration, baseline_output) =
        run_cargo_test(Duration::from_secs(300)).await;
    if !baseline_ok {
        eprintln!("❌ Baseline tests failed! Fix your tests first.\n{baseline_output}");
        return Ok(());
    }
    let test_timeout = baseline_duration.mul_f64(3.0).max(Duration::from_secs(30));
    eprintln!(
        "   ✅ Baseline passed in {:.1}s (mutation timeout: {:.1}s)",
        baseline_duration.as_secs_f64(),
        test_timeout.as_secs_f64()
    );

    // Step 4: Ask AI for mutation plan
    eprintln!("\n🧬 Asking AI for mutation plan...");
    let aws_config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let bedrock = aws_sdk_bedrockruntime::Client::new(&aws_config);

    let plan_text = converse(
        &bedrock,
        model_id,
        "You are a mutation testing expert for Rust. Respond only with valid JSON.",
        &build_mutation_prompt(&file_contents),
    )
    .await?;

    debug!("AI mutation plan: {plan_text}");

    let plan: MutationPlan = serde_json::from_str(strip_code_fences(&plan_text))
        .map_err(|e| format!("Failed to parse mutation plan: {e}\nRaw response:\n{plan_text}"))?;

    eprintln!("   Got {} mutations", plan.mutations.len());

    // Step 5: Test each mutation
    eprintln!("\n🧪 Testing mutations...\n");
    let results = run_mutations(&cwd, plan.mutations, test_timeout).await;

    // Step 6: Summarize results
    let survived_count = results
        .iter()
        .filter(|r| matches!(r.outcome, MutationOutcome::Survived))
        .count();
    let killed = results
        .iter()
        .filter(|r| matches!(r.outcome, MutationOutcome::Killed))
        .count();
    let total_testable = results
        .iter()
        .filter(|r| {
            !matches!(
                r.outcome,
                MutationOutcome::BuildError(_) | MutationOutcome::LineMismatch(_)
            )
        })
        .count();

    eprintln!(
        "\n📊 Results: {killed} killed, {survived_count} survived out of {total_testable} testable mutations"
    );

    if survived_count == 0 {
        eprintln!("\n🎉 All mutations were killed! Your tests look solid.");
        return Ok(());
    }

    // Step 7: Ask AI for analysis and suggestions
    eprintln!("\n💡 Analyzing surviving mutations...\n");

    let results_summary = format_results_summary(&results);
    let system_prompt = if config.auto_mode {
        "Output only the updated file in the exact delimited format requested. No markdown. No explanations. No code fences."
    } else {
        "You are a Rust testing expert. Help improve test coverage based on mutation testing results."
    };
    let analysis = converse(
        &bedrock,
        model_id,
        system_prompt,
        &build_analysis_prompt(config.auto_mode, &results_summary, &file_contents),
    )
    .await?;

    println!("{analysis}");

    // Step 8: Auto-apply if requested
    if config.auto_mode {
        auto_apply(&cwd, &analysis, test_timeout).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_source_files() {
        let temp = std::env::temp_dir().join("morris_test_list");
        let src = temp.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "// lib").unwrap();
        std::fs::write(src.join("main.rs"), "// main").unwrap();
        std::fs::write(src.join("readme.txt"), "not rust").unwrap();

        let files = list_source_files(&temp);
        assert!(files.iter().any(|f| f.ends_with("lib.rs")));
        assert!(files.iter().any(|f| f.ends_with("main.rs")));
        assert!(!files.iter().any(|f| f.ends_with("readme.txt")));

        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn test_list_source_files_no_src() {
        let temp = std::env::temp_dir().join("morris_test_nosrc");
        std::fs::create_dir_all(&temp).unwrap();
        assert!(list_source_files(&temp).is_empty());
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn test_list_source_files_sorted() {
        let temp = std::env::temp_dir().join("morris_test_sorted");
        let src = temp.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("zebra.rs"), "").unwrap();
        std::fs::write(src.join("alpha.rs"), "").unwrap();

        let files = list_source_files(&temp);
        assert!(files.windows(2).all(|w| w[0] <= w[1]));

        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn test_find_target_line_exact() {
        let lines = vec!["fn a() {}", "    let x = 1;", "}"];
        assert_eq!(find_target_line(&lines, 2, "    let x = 1;"), Some(2));
    }

    #[test]
    fn test_find_target_line_trimmed() {
        let lines = vec!["fn a() {}", "    let x = 1;", "}"];
        assert_eq!(find_target_line(&lines, 2, "let x = 1;"), Some(2));
    }

    #[test]
    fn test_find_target_line_fuzzy() {
        let lines = vec![
            "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "target", "l",
        ];
        // Looking at line 3 but content is at line 11 (within ±10)
        assert_eq!(find_target_line(&lines, 3, "target"), Some(11));
    }

    #[test]
    fn test_find_target_line_not_found() {
        let lines = vec!["a", "b", "c"];
        assert_eq!(find_target_line(&lines, 2, "nonexistent"), None);
    }

    #[test]
    fn test_find_target_line_out_of_range() {
        let lines = vec!["a", "b"];
        assert_eq!(find_target_line(&lines, 0, "a"), None);
        assert_eq!(find_target_line(&lines, 3, "a"), None);
    }

    #[test]
    fn test_config_defaults() {
        let config = Config::default();
        assert!(!config.auto_mode);
        assert!(!config.quick_mode);
        assert!(!config.verbose);
    }

    #[test]
    fn test_config_model_id() {
        let mut config = Config::default();
        assert_eq!(config.model_id(), "us.anthropic.claude-sonnet-4-6");
        config.quick_mode = true;
        assert_eq!(
            config.model_id(),
            "us.anthropic.claude-haiku-4-5-20251001-v1:0"
        );
    }

    #[test]
    fn test_mutation_outcome_display() {
        assert_eq!(MutationOutcome::Survived.to_string(), "SURVIVED");
        assert_eq!(MutationOutcome::Killed.to_string(), "KILLED");
        assert_eq!(MutationOutcome::Timeout.to_string(), "TIMEOUT");
        assert_eq!(
            MutationOutcome::BuildError("oops".into()).to_string(),
            "BUILD ERROR: oops"
        );
        assert_eq!(
            MutationOutcome::LineMismatch("bad".into()).to_string(),
            "LINE MISMATCH: bad"
        );
    }

    #[tokio::test]
    async fn test_run_cargo_test_timeout() {
        // This just verifies the timeout mechanism works - it won't actually
        // run cargo test successfully outside a real project
        let (_, _, output) = run_cargo_test(Duration::from_millis(1)).await;
        // Either times out or fails fast - both are fine
        assert!(!output.is_empty());
    }

    #[tokio::test]
    async fn test_line_mutation_restore() {
        let temp = std::env::temp_dir().join("morris_test_restore.rs");
        std::fs::write(&temp, "fn test() {\n    let x = 1;\n}\n").unwrap();

        let _ = test_line_mutation(
            temp.to_str().unwrap(),
            2,
            "    let x = 1;",
            "    let x = 2;",
            Duration::from_secs(5),
        )
        .await;

        let content = std::fs::read_to_string(&temp).unwrap();
        assert!(content.contains("let x = 1;"), "file should be restored");
        std::fs::remove_file(temp).unwrap();
    }

    #[tokio::test]
    async fn test_line_mutation_mismatch() {
        let temp = std::env::temp_dir().join("morris_test_mismatch.rs");
        std::fs::write(&temp, "fn test() {\n    let x = 1;\n}\n").unwrap();

        let outcome = test_line_mutation(
            temp.to_str().unwrap(),
            2,
            "    let x = 999;",
            "    let x = 2;",
            Duration::from_secs(5),
        )
        .await;

        assert!(matches!(outcome, MutationOutcome::LineMismatch(_)));
        std::fs::remove_file(temp).unwrap();
    }

    #[test]
    fn test_find_target_line_last_line() {
        let lines = vec!["a", "b", "c"];
        assert_eq!(find_target_line(&lines, 3, "c"), Some(3));
    }

    #[test]
    fn test_find_target_line_fuzzy_boundary() {
        // Target exactly 10 lines above the hint — must still find it
        let mut lines = vec!["x"; 21];
        lines[10] = "target"; // line 11
        assert_eq!(find_target_line(&lines, 1, "target"), Some(11));

        // Target exactly 10 lines below the hint — must still find it
        let mut lines2 = vec!["x"; 25];
        lines2[1] = "target"; // line 2
        assert_eq!(find_target_line(&lines2, 12, "target"), Some(2));

        // Target 11 lines away — outside window, must NOT find it
        let mut lines3 = vec!["x"; 25];
        lines3[0] = "target"; // line 1
        assert_eq!(find_target_line(&lines3, 12, "target"), None);
    }

    #[test]
    fn test_read_all_sources_line_numbers() {
        let temp = std::env::temp_dir().join("morris_test_lnums");
        let src = temp.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "line_one\nline_two\nline_three\n").unwrap();

        let files = list_source_files(&temp);
        let contents = read_all_sources(&temp, &files).unwrap();

        assert!(
            contents.contains("   1| line_one"),
            "first line must be 1-based"
        );
        assert!(contents.contains("   2| line_two"), "second line must be 2");
        assert!(
            contents.contains("   3| line_three"),
            "third line must be 3"
        );
        assert!(
            !contents.contains("   0| "),
            "must not contain 0-based numbers"
        );

        std::fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn test_timeout_multiplier() {
        let baseline = Duration::from_secs(10);
        let timeout = baseline.mul_f64(3.0).max(Duration::from_secs(30));
        assert_eq!(timeout, Duration::from_secs(30));

        let baseline2 = Duration::from_secs(20);
        let timeout2 = baseline2.mul_f64(3.0).max(Duration::from_secs(30));
        assert_eq!(timeout2, Duration::from_secs(60));
        // With multiplier 2.0 this would be 40, not 60
        assert_ne!(timeout2, Duration::from_secs(40));
    }

    #[tokio::test]
    async fn test_line_mutation_mismatch_last_line() {
        let temp = std::env::temp_dir().join("morris_test_mismatch_last.rs");
        std::fs::write(&temp, "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();

        let outcome = test_line_mutation(
            temp.to_str().unwrap(),
            3,
            "fn WRONG() {}",
            "fn d() {}",
            Duration::from_secs(5),
        )
        .await;

        match &outcome {
            MutationOutcome::LineMismatch(msg) => {
                assert!(
                    !msg.contains("out of range"),
                    "last line should not be out-of-range"
                );
                assert!(msg.contains("fn c()"), "should show the actual last line");
            }
            other => panic!("expected LineMismatch, got {other}"),
        }
        std::fs::remove_file(temp).unwrap();
    }
}
