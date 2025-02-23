mod gpt_validator;
mod vulnerability_checks;
use crate::vulnerability_checks::VulnerabilityCheck;
mod report_generator;
use gpt_validator::OpenAICreds;
use report_generator::{
    FinalReport, SafePatternDetail, SecurityAnalysisSummary, VulnerabilityResult,
};

use clap::{App, Arg};
use regex::Regex;
use std::{collections::HashMap, error::Error, fs};
use walkdir::WalkDir;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let matches = App::new("AI-driven Smart Contract Static Analyzer")
        .version("0.3")
        .author("YevhSec")
        .about("QBIT detects vulnerabilities in Smart Contracts based on patterns and AI code analysis. Currently supports Solana based on Rust and Ethereum based on Solidity.")
        .arg(Arg::with_name("folder_path")
             .help("The path to the folder to scan")
             .required(true)
             .index(1))
        .arg(Arg::with_name("all_severities")
             .long("all-severities")
             .help("Validate findings of all severities, not just critical and high"))
        .arg(Arg::with_name("model")
             .long("model")
             .value_name("MODEL")
             .help("OpenAI model GPT-3.5 or GPT-4 to use for vulnerability validation (default is gpt-3.5-turbo)")
             .takes_value(true))
        .arg(Arg::with_name("no_validation")
             .long("no-validation")
             .help("Skip vulnerability validation"))
        .get_matches();

    let folder_path = matches.value_of("folder_path").unwrap();
    let openai_creds = gpt_validator::OpenAICreds {
        api_key: std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set"),
        org_id: std::env::var("OPENAI_ORG_ID").ok(),
        project_id: std::env::var("OPENAI_PROJECT_ID").ok(),
    };
    let validate_all_severities = matches.is_present("all_severities");
    let model = matches.value_of("model").unwrap_or("gpt-3.5-turbo");
    let no_validation = matches.is_present("no_validation");

    let vulnerability_checks = vulnerability_checks::initialize_vulnerability_checks();
    let results_by_language = analyze_folder(
        folder_path,
        &openai_creds,
        &vulnerability_checks[..],
        validate_all_severities,
        model,
        no_validation,
    )
    .await?;

    for (language, (files_list, vulnerabilities_details, safe_patterns_map)) in
        results_by_language
    {
        let safe_patterns_overview: Vec<SafePatternDetail> = safe_patterns_map
            .into_iter()
            .map(|(_, detail)| detail)
            .collect();

        let report = FinalReport {
            security_analysis_summary: SecurityAnalysisSummary {
                checked_files: files_list.len(),
                files_list,
                security_issues_found: vulnerabilities_details.len(),
            },
            vulnerabilities_details,
            safe_patterns_overview,
            model: if no_validation { "-".to_string() } else { model.to_string() },
        };

        let html_content = report_generator::generate_html_report(&report, &language);
        fs::write(format!("{}_QBIT_SAST_Report.html", language), html_content)
            .expect("Unable to write HTML report");
    }

    Ok(())
}

async fn analyze_folder(
    folder_path: &str,
    openai_creds: &OpenAICreds,
    checks: &[VulnerabilityCheck],
    validate_all_severities: bool,
    model: &str,
    no_validation: bool,
) -> Result<
    HashMap<
        String,
        (
            Vec<String>,
            Vec<VulnerabilityResult>,
            HashMap<String, SafePatternDetail>,
        ),
    >,
    Box<dyn Error>,
> {
    let mut results_by_language: HashMap<
        String,
        (
            Vec<String>,
            Vec<VulnerabilityResult>,
            HashMap<String, SafePatternDetail>,
        ),
    > = HashMap::new();

    for entry in WalkDir::new(folder_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let ext = e.path().extension().and_then(|e| e.to_str()).unwrap_or("");
            ext == "rs" || ext == "sol"
        })
    {
        let path = entry.path();
        let language = match path.extension().and_then(|e| e.to_str()) {
            Some("rs") => "Rust",
            Some("sol") => "Solidity-Ethereum",
            _ => continue,
        };

        let file_content = fs::read_to_string(path)?;
        let (files_list, vulnerabilities_details, safe_patterns_overview) = results_by_language
            .entry(language.to_string())
            .or_insert_with(|| (Vec::new(), Vec::new(), HashMap::new()));

        files_list.push(path.to_string_lossy().to_string());

        // Group findings per file
        let mut findings_by_file = Vec::new();

        for (line_number, line) in file_content.lines().enumerate() {
            for check in checks.iter().filter(|c| c.language == language) {
                let pattern_regex = Regex::new(&check.pattern)?;
                let safe_pattern_regex = check
                    .safe_pattern
                    .as_ref()
                    .and_then(|sp| Regex::new(sp).ok());

                if pattern_regex.is_match(line) {
                    findings_by_file.push((
                        line_number + 1,
                        check.id.clone(),
                        check.severity.clone(),
                        check.suggested_fix.clone(),
                    ));
                }

                if let Some(safe_regex) = &safe_pattern_regex {
                    if safe_regex.is_match(line) {
                        let entry = safe_patterns_overview
                            .entry(check.id.clone())
                            .or_insert_with(|| SafePatternDetail {
                                pattern_id: check.id.clone(),
                                title: check.title.clone(),
                                safe_pattern: check.safe_pattern.clone().unwrap_or_default(),
                                occurrences: 0,
                                files: vec![],
                            });

                        entry.occurrences += 1;
                        if !entry.files.contains(&path.to_string_lossy().to_string()) {
                            entry.files.push(path.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }

        let status = if no_validation {
            "-".to_string()
        } else {
            let (status, _) = gpt_validator::validate_vulnerabilities_with_gpt(
                openai_creds,
                &findings_by_file,
                &file_content,
                language,
                validate_all_severities,
                model,
            )
            .await?;
            status
        };

        for (line_number, vulnerability_id, severity, suggested_fix) in findings_by_file {
            vulnerabilities_details.push(VulnerabilityResult {
                vulnerability_id: vulnerability_id.clone(),
                file: path.to_string_lossy().to_string(),
                line_number,
                title: checks
                    .iter()
                    .find(|c| c.id == vulnerability_id)
                    .unwrap()
                    .title
                    .clone(),
                severity,
                status: status.clone(),
                description: checks
                    .iter()
                    .find(|c| c.id == vulnerability_id)
                    .unwrap()
                    .description
                    .clone(),
                fix: suggested_fix,
                persistence_of_safe_pattern: "No".to_string(),
                safe_pattern: None,
            });
        }
    }

    Ok(results_by_language)
}
