//! The reusable workflow (M14) and the recipes that call it: its inputs, its jobs in the
//! recipes' structure, and what pinning it by commit pins. CI runs actionlint on all of them.

use serde_json::Value;

fn yaml(relative: &str) -> Value {
    let path = hushrun_testkit::workspace_root().join(relative);
    let text = std::fs::read_to_string(&path).unwrap();
    serde_saphyr::from_str(&text).unwrap_or_else(|e| panic!("{relative}: {e}"))
}

fn steps(job: &Value) -> Vec<&Value> {
    job["steps"].as_array().unwrap().iter().collect()
}

/// The `run` scripts of a job's steps, in order.
fn scripts(job: &Value) -> Vec<String> {
    steps(job)
        .iter()
        .filter_map(|s| s["run"].as_str().map(str::to_string))
        .collect()
}

fn position(scripts: &[String], needle: &str) -> usize {
    scripts
        .iter()
        .position(|s| s.contains(needle))
        .unwrap_or_else(|| panic!("no step runs {needle:?}: {scripts:#?}"))
}

// covers: T-4
#[test]
fn the_reusable_workflow_carries_the_recipes_structure() {
    let workflow = yaml(".github/workflows/hushrun-deploy.yml");
    let call = &workflow["on"]["workflow_call"];
    assert_eq!(call["inputs"]["deploy"]["required"], true);
    // [ADR 0062] One pipeline: no stage to name, and nothing in the jobs passes one.
    assert!(call["inputs"].get("stage").is_none());
    // [ADR 0035] Unpinned, hushrun is the newest release.
    assert_eq!(call["inputs"]["version"]["default"], "");
    assert!(call["inputs"]["version"]["required"].is_null());
    for optional in [
        "build",
        "image",
        "scope",
        "force-full",
        "isolated",
        "repository-base",
        "repository-url",
        "toolchain",
        "signing",
    ] {
        assert!(call["inputs"][optional].is_object(), "{optional}");
    }
    // Every environment that holds the signing key also says how to sign (T-12).
    let mut signing = 0;
    for job in workflow["jobs"].as_object().unwrap().values() {
        let steps = job["steps"].as_array().into_iter().flatten();
        for env in std::iter::once(&job["env"]).chain(steps.map(|step| &step["env"])) {
            if env["HUSHRUN_SIGNING_KEY"].is_string() {
                assert_eq!(env["HUSHRUN_SIGNING"], "${{ inputs.signing }}", "{job}");
                signing += 1;
            }
        }
    }
    assert_eq!(signing, 5);
    assert_eq!(call["inputs"]["image"]["default"], "image");
    // The repository's URL comes from settings, never from the tree (T-4).
    assert_eq!(
        workflow["env"]["HUSHRUN_REPOSITORY_BASE"],
        "${{ inputs.repository-base || vars.HUSHRUN_REPOSITORY_BASE }}"
    );
    assert_eq!(
        workflow["env"]["HUSHRUN_REPOSITORY_URL"],
        "${{ inputs.repository-url || vars.HUSHRUN_REPOSITORY_URL }}"
    );
    // The names before ADR 0029 are gone: nothing was released with them (ADR 0030).
    assert!(workflow["env"].get("HUSHRUN_LEDGER").is_none());
    assert!(call["inputs"].get("ledger").is_none());
    assert!(call["inputs"].get("ledger-base").is_none());

    let jobs = workflow["jobs"].as_object().unwrap();
    assert_eq!(
        jobs.keys().map(String::as_str).collect::<Vec<_>>(),
        [
            "deploy",
            "plan",
            "report-cancelled",
            "report-failed",
            "test"
        ]
    );
    for (name, job) in jobs {
        for script in scripts(job) {
            assert!(
                !script.contains("--stage") && !script.contains("$STAGE"),
                "{name}: {script}"
            );
        }
    }
    assert!(
        scripts(&jobs["plan"])
            .iter()
            .any(|s| s.contains("hushrun plan --start ${SCOPE:+--scope \"$SCOPE\"}"))
    );
    // The test job runs exactly the plan's checks_to_run: a bare `run` with the plan.
    assert_eq!(
        jobs["test"]["env"]["HUSHRUN_PLAN_JSON"],
        "${{ needs.plan.outputs.json }}"
    );
    assert!(
        scripts(&jobs["test"])
            .iter()
            .any(|s| s.starts_with("hushrun run ${SCOPE:+--scope \"$SCOPE\"}"))
    );
    // deploy: guard, resolve, build and attest, deploy and mark deployed, mark passed, in that
    // order. [ADR 0062] No deployment checks run between the deploy and its `passed`.
    let deploy = scripts(&jobs["deploy"]);
    assert!(
        !deploy.iter().any(|s| s.contains("hushrun run")),
        "{deploy:#?}"
    );
    let order = [
        "hushrun guard",
        "hushrun artifact resolve",
        "hushrun attest",
        "hushrun mark deployed",
        "hushrun mark passed",
    ]
    .map(|needle| position(&deploy, needle));
    assert!(
        order.windows(2).all(|w| w[0] < w[1]),
        "{order:?}: {deploy:#?}"
    );
    assert!(
        scripts(&jobs["report-cancelled"])
            .iter()
            .any(|s| s.contains("mark superseded"))
    );
    assert!(
        scripts(&jobs["report-failed"])
            .iter()
            .any(|s| s.contains("mark failed"))
    );
    assert!(
        jobs["deploy"]["if"]
            .as_str()
            .unwrap()
            .contains(r#"["promote","refresh","full"]"#)
    );

    // Every job installs one release, checked against its SHA-256; the anchor gives each the
    // same script. [ADR 0035] The plan job installs the caller's version, else the newest
    // release, and every later job installs the one it did, even when a newer one comes out
    // meanwhile.
    assert_eq!(workflow["env"]["HUSHRUN_VERSION"], "${{ inputs.version }}");
    assert_eq!(
        jobs["plan"]["outputs"]["version"],
        "${{ steps.install.outputs.version }}"
    );
    for (name, job) in jobs {
        let install = scripts(job)
            .into_iter()
            .find(|s| s.contains("releases/download"))
            .unwrap_or_else(|| panic!("{name} installs hushrun"));
        assert!(
            install.contains("sha256sum")
                && install.contains("$HUSHRUN_VERSION")
                && install.contains("\"$releases/latest\"")
                && install.contains("echo \"version=$HUSHRUN_VERSION\" >> \"$GITHUB_OUTPUT\""),
            "{name}"
        );
        if name != "plan" {
            assert_eq!(
                job["env"]["HUSHRUN_VERSION"], "${{ needs.plan.outputs.version }}",
                "{name}"
            );
            assert!(
                job["needs"].to_string().contains("plan"),
                "{name} needs the plan job"
            );
        }
    }

    // Pinning the workflow by commit pins everything it runs: every action by its commit.
    for (name, job) in jobs {
        for step in steps(job) {
            if let Some(uses) = step["uses"].as_str() {
                let (_, reference) = uses.split_once('@').unwrap();
                assert!(
                    reference.len() == 40 && reference.bytes().all(|b| b.is_ascii_hexdigit()),
                    "{name}: {uses} isn't pinned by commit"
                );
            }
        }
    }
}

#[test]
fn the_recipes_call_the_reusable_workflow() {
    for file in ["recipes/github/hushrun.yml", "recipes/github/monorepo.yml"] {
        let recipe = yaml(file);
        let job = &recipe["jobs"]["deploy"];
        assert!(
            job["uses"]
                .as_str()
                .unwrap()
                .starts_with("hushrun/hushrun/.github/workflows/hushrun-deploy.yml@"),
            "{file}"
        );
        // [ADR 0062] One caller deploys the one pipeline: no stage, and no workflow that waits
        // for another's run.
        assert!(job["with"].get("stage").is_none(), "{file}");
        assert!(recipe["on"].get("workflow_run").is_none(), "{file}");
        // Pushes to the branch it deploys from plan; a push of a digest main runs is current.
        assert_eq!(
            recipe["on"]["push"]["branches"],
            serde_json::json!(["main"]),
            "{file}"
        );
        assert_eq!(
            job["with"]["force-full"], "${{ inputs.force_full || false }}",
            "{file}"
        );
        // [ADR 0035] Each run installs the newest release.
        assert!(job["with"]["version"].is_null(), "{file}");
        assert_eq!(
            job["with"]["repository-base"], "${{ vars.HUSHRUN_REPOSITORY_BASE }}",
            "{file}"
        );
        assert_eq!(recipe["permissions"]["id-token"], "write", "{file}");
        // One deploy at a time on main.
        assert!(
            recipe["concurrency"]["group"]
                .as_str()
                .unwrap()
                .contains("'deploy'"),
            "{file}"
        );
    }
    // A monorepo calls it once per scope, each with its scope's artifact.
    let monorepo = yaml("recipes/github/monorepo.yml");
    let job = &monorepo["jobs"]["deploy"];
    assert_eq!(
        job["strategy"]["matrix"]["scope"],
        serde_json::json!(["api", "web"])
    );
    assert_eq!(job["with"]["scope"], "${{ matrix.scope }}");
    assert_eq!(job["with"]["image"], "${{ matrix.scope }}-image");
    let monitor = yaml("recipes/github/monitor.yml");
    let setup = steps(&monitor["jobs"]["monitor"])
        .into_iter()
        .find(|step| {
            step["uses"]
                .as_str()
                .is_some_and(|uses| uses.starts_with("hushrun/hushrun/action/setup@"))
        })
        .expect("the monitor sets hushrun up");
    assert!(setup["with"]["version"].is_null(), "{setup}");
    // The recipes of one caller per stage are gone.
    for gone in [
        "recipes/github/staging.yml",
        "recipes/github/production.yml",
    ] {
        assert!(
            !hushrun_testkit::workspace_root().join(gone).exists(),
            "{gone}"
        );
    }
}
