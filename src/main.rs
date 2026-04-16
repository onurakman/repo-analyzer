use clap::Parser;

mod cli;
mod git;
mod metrics;
mod output;
mod parser;
mod pipeline;
mod types;

use output::ReportWriter;
use types::{OutputConfig, OutputFormat};

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();

    let time_range = cli.parse_time_range()?;
    let report_kinds = cli.parse_report_kinds()?;

    // Validate repo path
    let repo_path = std::path::Path::new(&cli.path);
    if !repo_path.join(".git").exists() && !repo_path.is_dir() {
        anyhow::bail!("'{}' is not a valid git repository", cli.path);
    }

    let registry = parser::registry::LanguageRegistry::build_default();

    // Convert threads: 0 means auto (None), positive means explicit count
    let threads = if cli.threads == 0 {
        None
    } else {
        Some(cli.threads)
    };

    let pipeline_config = pipeline::engine::PipelineConfig {
        repo_path: cli.path.clone(),
        time_range,
        report_kinds,
        quiet: cli.quiet,
        threads,
    };

    let pipeline = pipeline::engine::Pipeline::new(pipeline_config, registry);
    let results = pipeline.run()?;

    let output_config = OutputConfig {
        format: cli.format.clone(),
        output_path: cli.output.clone(),
        top: cli.top,
        quiet: cli.quiet,
    };

    let writer: Box<dyn ReportWriter> = match cli.format {
        OutputFormat::Table => Box::new(output::terminal::TerminalWriter),
        OutputFormat::Json => Box::new(output::json::JsonWriter),
        OutputFormat::Csv => Box::new(output::csv_output::CsvWriter),
        OutputFormat::Html => Box::new(output::html::HtmlWriter),
    };

    writer.write(&results, &output_config)?;

    Ok(())
}
