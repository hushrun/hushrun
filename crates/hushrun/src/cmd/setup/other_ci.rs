//! \[ADR 0047\] CI whose files init doesn't edit: CircleCI, Azure Pipelines, Buildkite, Jenkins and
//! AppVeyor. A run there reads none of them, so it can't see a later step that reads the reports a
//! skip wouldn't write (C-26). init reads them as text, for one thing only: a package.json script
//! that writes reports, run in a file that reads reports, stays as it is.

use std::path::Path;

/// The files, from the repository's root.
const FILES: &[&str] = &[
    "azure-pipelines.yml",
    ".azure-pipelines.yml",
    "Jenkinsfile",
    "appveyor.yml",
    ".appveyor.yml",
];

/// The directories whose YAML files are pipelines, one level deep.
const DIRECTORIES: &[&str] = &[
    ".circleci",
    ".buildkite",
    ".azure-pipelines",
    "azure-pipelines",
];

/// What says a file reads reports, ignoring case.
const READERS: &[&str] = &[
    "store_test_results",
    "publishtestresults",
    "publishcodecoverageresults",
    "codecov",
    "coveralls",
    "junit",
];

/// The largest file read: a pipeline is never this big.
const MOST_BYTES: u64 = 1 << 20;

/// A CI file init doesn't edit that reads reports, and the package.json scripts its lines run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// Its path from the root, `/`-separated.
    pub path: String,
    pub scripts: Vec<String>,
}

/// The CI files of `root` that init doesn't edit and that read reports.
pub fn reading(root: &Path) -> Vec<Reading> {
    let mut paths: Vec<String> = FILES.iter().map(|file| (*file).to_string()).collect();
    for dir in DIRECTORIES {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .filter(|name| name.ends_with(".yml") || name.ends_with(".yaml"))
            .collect();
        names.sort();
        paths.extend(names.into_iter().map(|name| format!("{dir}/{name}")));
    }
    paths
        .into_iter()
        .filter_map(|path| {
            let file = root.join(&path);
            let small = std::fs::metadata(&file)
                .is_ok_and(|meta| meta.is_file() && meta.len() <= MOST_BYTES);
            let text = small
                .then(|| std::fs::read_to_string(&file).ok())
                .flatten()?;
            read(&path, &text)
        })
        .collect()
}

/// What `text`, the file at `path`, says: whether it reads reports, and the scripts it runs.
fn read(path: &str, text: &str) -> Option<Reading> {
    let lower = text.to_ascii_lowercase();
    if !READERS.iter().any(|reader| lower.contains(reader)) {
        return None;
    }
    let mut scripts: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some(command) = command_in(line) else {
            continue;
        };
        for script in super::scripts::scripts_run(&command) {
            if !scripts.contains(&script) {
                scripts.push(script);
            }
        }
    }
    Some(Reading {
        path: path.to_string(),
        scripts,
    })
}

/// The shell command a line of a pipeline holds, from its package manager on: `- run: npm test`,
/// `command: yarn test`, `sh 'pnpm test'`, `script: npm run test:ci`. Quotes and brackets that
/// close the line go.
fn command_in(line: &str) -> Option<String> {
    let at = ["npm ", "pnpm ", "yarn ", "bun "]
        .iter()
        .filter_map(|manager| {
            line.match_indices(manager)
                .map(|(at, _)| at)
                .find(|at| *at == 0 || !line.as_bytes()[at - 1].is_ascii_alphanumeric())
        })
        .min()?;
    let rest = &line[at..];
    // A command in quotes, as `sh 'pnpm test'` writes it, ends where they close.
    let quote = line[..at]
        .trim_end()
        .chars()
        .last()
        .filter(|c| matches!(c, '\'' | '"'));
    let command = match quote.and_then(|quote| rest.find(quote)) {
        Some(end) => &rest[..end],
        None => rest
            .trim_end()
            .trim_end_matches([')', '\'', '"', '`', ',', ';']),
    };
    Some(command.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: C-26
    #[test]
    fn a_file_reads_reports_and_runs_scripts() {
        let circle = "jobs:\n  test:\n    steps:\n      - checkout\n      - run: npm ci\n      - run: npm test\n      - run:\n          command: yarn test:ci --coverage\n      - store_test_results:\n          path: reports\n";
        let found = read(".circleci/config.yml", circle).expect("reads reports");
        assert_eq!(found.scripts, ["test", "test:ci"]);
        let jenkins = "pipeline {\n  stages {\n    stage('Test') { steps { sh 'pnpm test' } }\n  }\n  post { always { junit 'reports/*.xml' } }\n}\n";
        assert_eq!(
            read("Jenkinsfile", jenkins).expect("reads").scripts,
            ["test"]
        );
        let azure = "steps:\n  - script: npm run test:unit\n  - task: PublishTestResults@2\n";
        assert_eq!(
            read("azure-pipelines.yml", azure).expect("reads").scripts,
            ["test:unit"]
        );
        // A file that reads none says nothing, whatever it runs.
        assert_eq!(
            read(".buildkite/pipeline.yml", "steps:\n  - command: npm test\n"),
            None
        );
        // A word that only ends like a package manager isn't one.
        assert_eq!(command_in("      - run: xnpm test"), None);
    }

    // covers: C-26
    #[test]
    fn the_files_are_found_where_each_ci_keeps_them() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".circleci")).unwrap();
        std::fs::write(
            root.join(".circleci/config.yml"),
            "jobs:\n  t:\n    steps:\n      - run: npm test\n      - store_test_results: {path: out}\n",
        )
        .unwrap();
        std::fs::write(root.join("Jenkinsfile"), "sh 'npm test'\n").unwrap();
        std::fs::create_dir_all(root.join(".github/workflows")).unwrap();
        std::fs::write(
            root.join(".github/workflows/ci.yml"),
            "codecov junit npm test\n",
        )
        .unwrap();
        assert_eq!(
            reading(root),
            [Reading {
                path: ".circleci/config.yml".into(),
                scripts: vec!["test".into()],
            }]
        );
    }
}
