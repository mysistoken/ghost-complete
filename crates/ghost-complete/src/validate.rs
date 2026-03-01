use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Resolve spec directories using the same heuristics as the proxy:
/// explicit config paths first, then ~/.config/ghost-complete/specs.
fn resolve_spec_dirs(config: &gc_config::GhostConfig) -> Vec<PathBuf> {
    if !config.paths.spec_dirs.is_empty() {
        return config
            .paths
            .spec_dirs
            .iter()
            .map(|s| {
                if s.starts_with('~') {
                    if let Some(home) = dirs::home_dir() {
                        return home.join(s.strip_prefix("~/").unwrap_or(s));
                    }
                }
                PathBuf::from(s)
            })
            .collect();
    }

    let mut dirs = Vec::new();
    if let Some(config_dir) = gc_config::config_dir() {
        dirs.push(config_dir.join("specs"));
    }
    dirs
}

fn count_spec_items(spec: &gc_suggest::CompletionSpec) -> (usize, usize) {
    fn count_subcommands(subs: &[gc_suggest::specs::SubcommandSpec]) -> usize {
        let mut n = subs.len();
        for sub in subs {
            n += count_subcommands(&sub.subcommands);
        }
        n
    }

    let subcommands = count_subcommands(&spec.subcommands);
    let options = spec.options.len();
    (subcommands, options)
}

fn validate_dir(dir: &Path) -> Result<(usize, usize)> {
    let mut valid = 0;
    let mut failed = 0;

    if !dir.exists() {
        println!("  Directory does not exist: {}\n", dir.display());
        return Ok((0, 0));
    }

    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read directory: {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();

        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                println!("  \x1b[31mFAIL\x1b[0m  {file_name}: {e}");
                failed += 1;
                continue;
            }
        };

        match serde_json::from_str::<gc_suggest::CompletionSpec>(&contents) {
            Ok(spec) => {
                let (subs, opts) = count_spec_items(&spec);
                println!("  \x1b[32m OK \x1b[0m  {file_name} ({subs} subcommands, {opts} options)");
                valid += 1;
            }
            Err(e) => {
                println!("  \x1b[31mFAIL\x1b[0m  {file_name}: {e}");
                failed += 1;
            }
        }
    }

    Ok((valid, failed))
}

pub fn run_validate_specs(config_path: Option<&str>) -> Result<()> {
    let config = gc_config::GhostConfig::load(config_path).context("failed to load config")?;

    let dirs = resolve_spec_dirs(&config);
    let mut total_valid = 0;
    let mut total_failed = 0;

    for dir in &dirs {
        println!("Validating specs in {}\n", dir.display());
        let (valid, failed) = validate_dir(dir)?;
        total_valid += valid;
        total_failed += failed;
    }

    if dirs.is_empty() {
        println!("No spec directories found.");
        return Ok(());
    }

    let total = total_valid + total_failed;
    println!();
    if total_failed == 0 {
        println!("{total}/{total} specs valid.");
    } else {
        println!("{total_valid}/{total} specs valid, {total_failed} failed.");
    }

    if total_failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
