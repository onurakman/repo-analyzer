use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// Create a temporary git repository with two commits for testing.
fn create_test_repo() -> TempDir {
    let dir = TempDir::new().expect("failed to create temp dir");
    let path = dir.path();

    // git init
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("git init failed");

    // Configure user
    std::process::Command::new("git")
        .args(["config", "user.email", "test@example.com"])
        .current_dir(path)
        .output()
        .expect("git config email failed");

    std::process::Command::new("git")
        .args(["config", "user.name", "Test User"])
        .current_dir(path)
        .output()
        .expect("git config name failed");

    // Create initial file and commit
    fs::write(
        path.join("hello.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .expect("write file failed");

    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .expect("git add failed");

    std::process::Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(path)
        .output()
        .expect("git commit failed");

    // Modify file and create second commit
    fs::write(
        path.join("hello.rs"),
        "fn main() {\n    println!(\"hello world\");\n}\n\nfn greet() {\n    println!(\"hi\");\n}\n",
    )
    .expect("write file failed");

    std::process::Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .expect("git add failed");

    std::process::Command::new("git")
        .args(["commit", "-m", "Add greet function"])
        .current_dir(path)
        .output()
        .expect("git commit failed");

    dir
}

#[test]
fn test_cli_help() {
    Command::cargo_bin("repo-analyzer")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("repository"));
}

#[test]
fn test_cli_version() {
    Command::cargo_bin("repo-analyzer")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("repo-analyzer"));
}

#[test]
fn test_cli_invalid_path() {
    Command::cargo_bin("repo-analyzer")
        .unwrap()
        .arg("/nonexistent/path")
        .arg("-q")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not a valid git repository"));
}

#[test]
fn test_cli_json_output() {
    let repo = create_test_repo();

    Command::cargo_bin("repo-analyzer")
        .unwrap()
        .arg(repo.path())
        .args(["-f", "json", "-q", "--only", "authors"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Test User"));
}

#[test]
fn test_cli_csv_output_to_file() {
    let repo = create_test_repo();
    let out_dir = TempDir::new().expect("failed to create output temp dir");
    let out_file = out_dir.path().join("report.csv");

    Command::cargo_bin("repo-analyzer")
        .unwrap()
        .arg(repo.path())
        .args([
            "-f",
            "csv",
            "-q",
            "--only",
            "authors",
            "-o",
            out_file.to_str().unwrap(),
        ])
        .assert()
        .success();

    let contents = fs::read_to_string(&out_file).expect("failed to read output file");
    assert!(
        contents.contains("Test User"),
        "CSV output should contain 'Test User', got: {contents}"
    );
}

#[test]
fn test_cli_html_output_to_file() {
    let repo = create_test_repo();
    let out_dir = TempDir::new().expect("failed to create output temp dir");
    let out_file = out_dir.path().join("report.html");

    Command::cargo_bin("repo-analyzer")
        .unwrap()
        .arg(repo.path())
        .args(["-f", "html", "-q", "-o", out_file.to_str().unwrap()])
        .assert()
        .success();

    let contents = fs::read_to_string(&out_file).expect("failed to read output file");
    assert!(
        contents.contains("repo-analyzer"),
        "HTML output should contain 'repo-analyzer', got: {contents}"
    );
}

#[test]
fn test_cli_since_filter() {
    let repo = create_test_repo();

    Command::cargo_bin("repo-analyzer")
        .unwrap()
        .arg(repo.path())
        .args(["--since", "1d", "--only", "authors", "-f", "json", "-q"])
        .assert()
        .success();
}

#[test]
fn test_cli_conflicting_time_args() {
    let repo = create_test_repo();

    Command::cargo_bin("repo-analyzer")
        .unwrap()
        .arg(repo.path())
        .args([
            "--since",
            "7d",
            "--from",
            "2024-01-01",
            "--to",
            "2024-12-31",
            "-q",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Cannot combine --since with --from/--to",
        ));
}

#[test]
fn test_cli_invalid_only_value() {
    let repo = create_test_repo();

    Command::cargo_bin("repo-analyzer")
        .unwrap()
        .arg(repo.path())
        .args(["--only", "invalid_report", "-q"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Unknown report kind"));
}

#[test]
fn test_cli_quiet_mode() {
    let repo = create_test_repo();

    Command::cargo_bin("repo-analyzer")
        .unwrap()
        .arg(repo.path())
        .args(["-q", "-f", "json", "--only", "authors"])
        .assert()
        .success();
}
