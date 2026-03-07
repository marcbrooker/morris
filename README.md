# 🧬 Morris

### AI-Powered Mutation Testing for Rust

*Find the bugs hiding in your test suite*

[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![AWS Bedrock](https://img.shields.io/badge/AWS-Bedrock-FF9900.svg)](https://aws.amazon.com/bedrock/)
[![Claude](https://img.shields.io/badge/Claude-Sonnet%204.6-8A2BE2.svg)](https://www.anthropic.com/claude)

```
    ╔═══════════════════════════════════════╗
    ║  Your Code  →  Morris  →  Better Tests ║
    ╚═══════════════════════════════════════╝
```

---

## 🎯 What is Morris?

Morris is a cargo subcommand that uses **AWS Bedrock (Claude Sonnet 4.6)** to perform intelligent mutation testing on Rust projects. Instead of exhaustively testing thousands of mutations, Morris uses AI to strategically select 5-8 high-value mutations that are most likely to reveal gaps in your test coverage.

Morris follows a **fixed workflow** — file discovery, test execution, and mutation application are all handled by deterministic code. The AI is used only for two targeted tasks: selecting which mutations to try, and analyzing the results.

```
┌─────────────┐      ┌──────────────┐      ┌─────────────┐
│  Your Code  │ ───> │    Morris    │ ───> │  Test Gaps  │
│   + Tests   │      │ (Fixed Flow) │      │  + Fixes    │
└─────────────┘      └──────────────┘      └─────────────┘
                            │
                            ├─ Discovers files (deterministic)
                            ├─ Runs baseline tests (deterministic)
                            ├─ AI selects mutations (Bedrock)
                            ├─ Tests mutations (deterministic)
                            └─ AI analyzes results (Bedrock)
```

---

## 🚀 Quick Start

### Installation

```bash
cargo install --path .
```

### Prerequisites

- **AWS Bedrock** access with Claude Sonnet 4.6 enabled
- AWS credentials configured (via `~/.aws/credentials` or environment variables)
- A Rust project with tests

### Basic Usage

```bash
cd your-rust-project
cargo morris
```

That's it! Morris will analyze your code and report surviving mutations.

---

## 📋 How It Works

Morris uses a fixed, deterministic workflow. The AI (via AWS Bedrock Converse API) is called exactly twice: once to propose mutations, and once to analyze results.

```
┌─────────────────────────────────────────────────────────────┐
│                     Morris Workflow                         │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  1. 📁 Discovery (deterministic)                            │
│     └─ Recursively finds .rs files under src/              │
│                                                             │
│  2. 📖 Read Sources (deterministic)                         │
│     └─ Reads all source files into memory                  │
│                                                             │
│  3. ⏱️  Baseline (deterministic)                             │
│     └─ Runs `cargo test` to verify and measure timing      │
│                                                             │
│  4. 🧬 Mutation Plan (AI — Bedrock call #1)                 │
│     └─ Claude proposes 5-8 strategic mutations as JSON     │
│        • Operators: > → <, + → -, == → !=                  │
│        • Boundaries: 0 → 1, len() → len()-1                │
│        • Logic: && → ||, true → false                      │
│                                                             │
│  5. 🧪 Testing Loop (deterministic)                         │
│     For each mutation:                                      │
│     ├─ Materialize structured mutation blocks               │
│     ├─ Run tests (with 3x baseline timeout)                │
│     └─ Activate/deactivate each variant deterministically   │
│                                                             │
│  6. 📊 Results Summary (deterministic)                      │
│     └─ Counts killed / survived / build errors             │
│                                                             │
│  7. 💡 Analysis (AI — Bedrock call #2)                      │
│     └─ Claude explains surviving mutations and             │
│        suggests specific tests to catch them               │
│                                                             │
│  8. ✨ Auto Mode (optional, deterministic)                  │
│     └─ Parses AI suggestions and writes improved tests     │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

---

## 🎛️ Command Line Options

| Flag               | Description                           | Use Case              |
| ------------------ | ------------------------------------- | --------------------- |
| *(none)*           | Default mode with Claude Sonnet 4.6   | Best quality analysis |
| `--quick`          | Use Claude Haiku 4.5                  | Faster, less thorough |
| `--auto`           | Automatically apply test improvements | Hands-free mode       |
| `-v` / `--verbose` | Enable debug logging                  | Troubleshooting       |

### Examples

```bash
# Standard analysis (recommended)
cargo morris

# Quick analysis for rapid feedback
cargo morris --quick

# Auto-apply test improvements
cargo morris --auto

# Quick + auto for maximum speed
cargo morris --quick --auto
```

---

## 📊 Example Output

```bash
$ cargo morris

🧬 Morris v0.2.0 - AI-Powered Mutation Testing

📁 Discovering source files...
   src/lib.rs

📖 Reading source files...
⏱️  Running baseline tests...
   ✅ Baseline passed in 1.2s (mutation timeout: 30.0s)

🧬 Asking AI for mutation plan...
   Got 6 mutations

🧪 Testing mutations...

   [1/6] src/lib.rs:42 - Change > to <... ❌ SURVIVED
   [2/6] src/lib.rs:67 - Change + to -... ✅ KILLED
   [3/6] src/lib.rs:89 - Change == to !=... ✅ KILLED
   [4/6] src/lib.rs:23 - Change >= to >... ✅ KILLED
   [5/6] src/lib.rs:51 - Change true to false... ❌ SURVIVED
   [6/6] src/lib.rs:15 - Remove bounds check... 🔧 BUILD ERROR

📊 Results: 2 killed, 3 survived out of 5 testable mutations

💡 Analyzing surviving mutations...

[AI analysis with specific test suggestions]
```

---

## 🆚 Morris vs cargo-mutants

[cargo-mutants](https://mutants.rs/) is an excellent exhaustive mutation testing tool. Morris takes a different approach:

**cargo-mutants — Exhaustive Approach**
- Systematically generates all possible mutations
- Tests hundreds/thousands of mutations
- AST-based pattern matching
- Comprehensive coverage analysis
- Best for: CI/CD pipelines, audits

**Morris — AI-Guided Approach**
- Fixed workflow, AI used only for selection & analysis
- Selects 5-8 strategic mutations
- Contextual explanations of why mutations survive
- Auto-applies improvements
- Best for: Interactive development, learning

The biggest difference is that mutants is a lot more mature, and probably more useful in production code bases for now.

---

## 🔧 Configuration

### AWS Credentials

Morris requires AWS credentials with Bedrock access:

```bash
# Option 1: AWS CLI
aws configure

# Option 2: Environment variables
export AWS_ACCESS_KEY_ID=your_key
export AWS_SECRET_ACCESS_KEY=your_secret
export AWS_REGION=us-east-1
```

### Verbose Output

```bash
# Enable debug logging
cargo morris -v

# Or via environment variable
RUST_LOG=debug cargo morris
```

---

## 🏗️ Architecture

Morris uses a fixed workflow with two targeted Bedrock Converse API calls. All file I/O, test execution, and mutation application is deterministic code — no agent loop or tool-use protocol.

```
┌──────────────────────────────────────────────────────────┐
│                     Morris Architecture                  │
├──────────────────────────────────────────────────────────┤
│                                                          │
│  ┌────────────┐                                         │
│  │   CLI      │  cargo morris [--quick] [--auto] [-v]   │
│  └─────┬──────┘                                         │
│        │                                                 │
│        v                                                 │
│  ┌────────────────────────────────────────┐             │
│  │        Fixed Workflow Engine            │             │
│  │                                        │             │
│  │  1. Discover .rs files (fs)            │             │
│  │  2. Read source files (fs)             │             │
│  │  3. Run baseline tests (cargo test)    │             │
│  │  4. Get mutation plan ──────────────┐  │             │
│  │  5. Test each mutation (cargo test) │  │             │
│  │  6. Summarize results               │  │             │
│  │  7. Get analysis ───────────────────┤  │             │
│  │  8. Auto-apply (optional, fs)       │  │             │
│  └─────────────────────────────────────┘  │             │
│                                           │             │
│                                           v             │
│                              ┌─────────────────────┐    │
│                              │  AWS Bedrock        │    │
│                              │  Converse API       │    │
│                              │  • Sonnet 4.6       │    │
│                              │  • Haiku 4.5        │    │
│                              │  (2 calls total)    │    │
│                              └─────────────────────┘    │
│                                                          │
└──────────────────────────────────────────────────────────┘
```
