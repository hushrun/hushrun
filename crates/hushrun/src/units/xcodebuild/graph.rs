//! \[ADR 0121\] What each test target of an Xcode project reads, from its `project.pbxproj` alone:
//! nothing is built, listed or resolved before COMMAND.
//!
//! A target reads the files its build phases name, the folders its file-system-synchronized
//! groups name (Xcode 16), the local Swift packages it uses, and all of that again for each target
//! it depends on: by a target dependency, by linking another target's product, or as the app that
//! hosts its tests (`TestTargetID`). What can't be followed is said, and makes the target `not
//! observed` (C-111).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use hushrun_core::units::{NotFollowed, Tree};

use super::pbxproj::{self, Value};
use crate::units::swift::KINDS;

/// The product types of a test bundle.
const TEST_TYPES: &[&str] = &[
    "com.apple.product-type.bundle.unit-test",
    "com.apple.product-type.bundle.ui-testing",
];

/// Whether `path` is of a kind Xcode compiles (the same kinds as SwiftPM's).
pub fn of_kind(path: &str) -> bool {
    KINDS.iter().any(|k| path.ends_with(k))
}

/// One test target of a project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestTarget {
    /// Its name, which `-skip-testing:` and the result bundle use.
    pub name: String,
    /// Every file of the scope it and the targets it depends on read.
    pub reads: BTreeSet<String>,
    /// Its own sources: the files of a compiled kind of its own phases and folders.
    pub own: Vec<String>,
    /// What can't be followed: the target is `not observed`.
    pub not_followed: BTreeSet<NotFollowed>,
}

/// \[C-111\] The test targets of the projects a run tests, and the sources they read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Graph {
    /// The test targets, by unit: `<project from the root>/<target>`.
    pub tests: BTreeMap<String, TestTarget>,
    /// Their units, by target name, which a scheme and a result bundle know them by.
    pub by_name: BTreeMap<String, String>,
    /// The sources (C-30): every file of a compiled kind some test target reads.
    pub named: BTreeSet<String>,
    /// Which test target reads which target's files, for `hushrun why`.
    pub edges: BTreeSet<crate::units::db::Edge>,
}

impl Graph {
    /// Whether `path` is a unit file: a test target's own source.
    #[cfg(test)]
    pub fn is_unit(&self, path: &str) -> bool {
        self.tests.values().any(|t| t.own.iter().any(|o| o == path))
    }

    /// Adds another project's targets.
    pub fn merge(&mut self, other: Graph) {
        self.tests.extend(other.tests);
        self.by_name.extend(other.by_name);
        self.named.extend(other.named);
        self.edges.extend(other.edges);
    }
}

/// Lexically normal form of `path`, with no `.` and each `..` taken away; `None` when a `..`
/// would go above where it starts.
fn normal(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

/// `path` (from the root) as a string with `/`, or `None` for one above the root.
fn rel_text(path: &Path) -> Option<String> {
    let normal = normal(path)?;
    let text = normal.to_str()?.replace('\\', "/");
    Some(text.trim_end_matches('/').to_string())
}

/// The files of `tree` at or below `dir` (`""` is the whole tree).
fn below<'t>(tree: &'t Tree, dir: &'t str) -> impl Iterator<Item = &'t str> {
    tree.paths().filter(move |p| {
        dir.is_empty() || *p == dir || p.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
    })
}

/// Where an object of the project is, from the root: a place in the checkout, a place outside
/// it, or none (a product, the SDK).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Place {
    In(String),
    Outside(String),
    Nowhere,
}

struct Project<'a> {
    objects: &'a BTreeMap<String, Value>,
    /// The project's folder, from the root.
    dir: String,
    /// Each group's child, to its group.
    parent: BTreeMap<&'a str, &'a str>,
    /// The root's absolute path, for `<absolute>` paths.
    root: &'a Path,
}

impl<'a> Project<'a> {
    fn object(&self, id: &str) -> Option<&'a Value> {
        self.objects.get(id)
    }

    fn isa(&self, id: &str) -> Option<&'a str> {
        self.object(id)?.str("isa")
    }

    /// Where the file, folder or group `id` is.
    fn place(&self, id: &str, depth: usize) -> Place {
        let Some(object) = self.object(id) else {
            return Place::Nowhere;
        };
        if depth > 64 {
            return Place::Nowhere;
        }
        let path = object.str("path").unwrap_or("");
        let base = match object.str("sourceTree").unwrap_or("<group>") {
            "<group>" => match self.parent.get(id) {
                Some(parent) => self.place(parent, depth + 1),
                None => Place::In(self.dir.clone()),
            },
            "SOURCE_ROOT" => Place::In(self.dir.clone()),
            "<absolute>" => {
                let absolute = Path::new(path);
                return match absolute.strip_prefix(self.root) {
                    Ok(rel) => rel_text(rel).map_or(Place::Outside(path.into()), Place::In),
                    Err(_) => Place::Outside(path.into()),
                };
            }
            _ => return Place::Nowhere,
        };
        match base {
            Place::In(dir) if path.is_empty() => Place::In(dir),
            Place::In(dir) => match rel_text(&Path::new(&dir).join(path)) {
                Some(p) => Place::In(p),
                None => Place::Outside(format!("{dir}/{path}")),
            },
            Place::Outside(dir) => Place::Outside(format!("{dir}/{path}")),
            Place::Nowhere => Place::Nowhere,
        }
    }
}

/// What one target reads by itself, before the targets it depends on.
#[derive(Debug, Default)]
struct Own {
    /// Files and folders of the checkout, from the root.
    places: BTreeSet<String>,
    /// Targets it depends on, by id.
    deps: BTreeSet<String>,
    not_followed: BTreeSet<NotFollowed>,
}

fn not_followed(why: &str, detail: impl Into<String>) -> NotFollowed {
    NotFollowed {
        why: why.into(),
        detail: detail.into(),
    }
}

/// \[C-111\] The graph of the project at `xcodeproj` (its folder, from the root), whose
/// `project.pbxproj` holds `text`, for the files `tree`; `root` is the checkout's absolute path.
/// `Err` when the file is no project.
#[cfg(test)]
pub fn graph_of(xcodeproj: &str, text: &str, tree: &Tree, root: &Path) -> Result<Graph, String> {
    graph_of_with(xcodeproj, text, tree, root, &[])
}

/// The graph of the project at `xcodeproj`, of a workspace that holds the Swift packages at
/// `local` (their folders, from the root): a package product the project names no package for is
/// one of them.
pub fn graph_of_with(
    xcodeproj: &str,
    text: &str,
    tree: &Tree,
    root: &Path,
    local: &[String],
) -> Result<Graph, String> {
    let list = pbxproj::parse(text).map_err(|e| format!("{xcodeproj}/project.pbxproj: {e}"))?;
    let objects = list
        .get("objects")
        .and_then(Value::as_dict)
        .ok_or_else(|| format!("{xcodeproj}/project.pbxproj has no objects"))?;
    let root_id = list
        .str("rootObject")
        .ok_or_else(|| format!("{xcodeproj}/project.pbxproj has no root object"))?;
    let project = objects.get(root_id).ok_or_else(|| {
        format!("{xcodeproj}/project.pbxproj names a root object it doesn't have")
    })?;
    let folder = Path::new(xcodeproj)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let dir = rel_text(&folder.join(project.str("projectDirPath").unwrap_or("")))
        .ok_or_else(|| format!("{xcodeproj}: its project folder is above the checkout"))?;
    let mut parent = BTreeMap::new();
    for (id, object) in objects {
        if matches!(
            object.str("isa"),
            Some("PBXGroup" | "PBXVariantGroup" | "XCVersionGroup")
        ) {
            for child in object.strs("children") {
                parent.insert(child, id.as_str());
            }
        }
    }
    let p = Project {
        objects,
        dir: dir.clone(),
        parent,
        root,
    };
    // Each product, to the target that makes it.
    let mut product_of: BTreeMap<&str, &str> = BTreeMap::new();
    let mut targets: Vec<&str> = Vec::new();
    for (id, object) in objects {
        if matches!(
            object.str("isa"),
            Some("PBXNativeTarget" | "PBXAggregateTarget" | "PBXLegacyTarget")
        ) {
            targets.push(id);
            if let Some(product) = object.str("productReference") {
                product_of.insert(product, id);
            }
        }
    }
    // Files a synchronized group's exceptions give another target.
    let mut synced_for: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (id, object) in objects {
        if object.str("isa") == Some("PBXFileSystemSynchronizedRootGroup") {
            for exception in object.strs("exceptions") {
                if let Some(target) = p.object(exception).and_then(|e| e.str("target")) {
                    synced_for.entry(target).or_default().insert(id);
                }
            }
        }
    }
    let host: BTreeMap<&str, &str> = project
        .get("attributes")
        .and_then(|a| a.get("TargetAttributes"))
        .and_then(Value::as_dict)
        .into_iter()
        .flatten()
        .filter_map(|(target, attrs)| Some((target.as_str(), attrs.str("TestTargetID")?)))
        .collect();
    let resolved = [
        format!("{xcodeproj}/project.xcworkspace/xcshareddata/swiftpm/Package.resolved"),
        format!("{dir}/Package.resolved"),
    ]
    .iter()
    .any(|p| tree.contains(p.trim_start_matches('/')));

    let mut own: BTreeMap<&str, Own> = BTreeMap::new();
    for &id in &targets {
        let target = &objects[id];
        let mut o = Own::default();
        if target.str("isa") == Some("PBXLegacyTarget") {
            o.places.insert(dir.clone());
        }
        for phase in target.strs("buildPhases") {
            let Some(phase_object) = p.object(phase) else {
                continue;
            };
            if phase_object.str("isa") == Some("PBXShellScriptBuildPhase") {
                // A script may read any file: the target reads its project's folder whole.
                o.places.insert(dir.clone());
                continue;
            }
            for file in phase_object.strs("files") {
                let Some(build_file) = p.object(file) else {
                    continue;
                };
                let Some(file_ref) = build_file.str("fileRef") else {
                    continue;
                };
                if let Some(maker) = product_of.get(file_ref) {
                    o.deps.insert((*maker).to_string());
                    continue;
                }
                let refs: Vec<&str> = if p.isa(file_ref) == Some("PBXVariantGroup") {
                    p.object(file_ref)
                        .map(|g| g.strs("children"))
                        .unwrap_or_default()
                } else {
                    vec![file_ref]
                };
                for r in refs {
                    match p.place(r, 0) {
                        Place::In(path) => {
                            o.places.insert(path);
                        }
                        Place::Outside(path) => {
                            o.not_followed.insert(not_followed("outside-scope", path));
                        }
                        Place::Nowhere => {}
                    }
                }
            }
        }
        let synced = target
            .strs("fileSystemSynchronizedGroups")
            .into_iter()
            .chain(synced_for.get(id).into_iter().flatten().copied());
        for group in synced {
            match p.place(group, 0) {
                Place::In(path) => {
                    o.places.insert(path);
                }
                Place::Outside(path) => {
                    o.not_followed.insert(not_followed("outside-scope", path));
                }
                Place::Nowhere => {}
            }
        }
        for dependency in target.strs("dependencies") {
            let Some(d) = p.object(dependency) else {
                continue;
            };
            if let Some(t) = d.str("target")
                && objects.contains_key(t)
            {
                o.deps.insert(t.to_string());
            } else {
                // A target of another project this one holds: not followed.
                let what = d
                    .str("name")
                    .or_else(|| {
                        d.str("targetProxy")
                            .and_then(|x| p.object(x))
                            .and_then(|x| x.str("remoteInfo"))
                    })
                    .unwrap_or(dependency);
                o.not_followed.insert(not_followed("another-project", what));
            }
        }
        if let Some(app) = host.get(id)
            && objects.contains_key(*app)
        {
            o.deps.insert((*app).to_string());
        }
        for product in target.strs("packageProductDependencies") {
            let Some(dep) = p.object(product) else {
                continue;
            };
            let package = dep.str("package").and_then(|x| p.object(x));
            match package.and_then(|x| x.str("isa")) {
                Some("XCLocalSwiftPackageReference") => {
                    let relative = package.and_then(|x| x.str("relativePath")).unwrap_or("");
                    match rel_text(&Path::new(&dir).join(relative)) {
                        Some(path) => {
                            o.places.insert(path);
                        }
                        None => {
                            o.not_followed
                                .insert(not_followed("outside-scope", relative.to_string()));
                        }
                    }
                }
                Some("XCRemoteSwiftPackageReference") => {
                    if !resolved {
                        o.not_followed.insert(not_followed(
                            "unpinned",
                            dep.str("productName").unwrap_or(product).to_string(),
                        ));
                    }
                }
                _ if !local.is_empty() => {
                    // A product of a package of the workspace, or added as a folder of the
                    // project: Xcode names no package for it, so it reads every one of them.
                    o.places.extend(local.iter().cloned());
                }
                _ => {
                    // A package added as a folder of the project: Xcode names no path for it.
                    o.not_followed.insert(not_followed(
                        "package-product",
                        dep.str("productName").unwrap_or(product).to_string(),
                    ));
                }
            }
        }
        own.insert(id, o);
    }

    let mut out = Graph::default();
    for &id in &targets {
        let target = &objects[id];
        if !TEST_TYPES.contains(&target.str("productType").unwrap_or("")) {
            continue;
        }
        let name = target.str("name").unwrap_or(id).to_string();
        let unit = format!("{xcodeproj}/{name}");
        let mut reads = BTreeSet::new();
        let mut not = BTreeSet::new();
        let mut seen = BTreeSet::new();
        let mut queue = vec![id.to_string()];
        while let Some(t) = queue.pop() {
            if !seen.insert(t.clone()) {
                continue;
            }
            let Some(o) = own.get(t.as_str()) else {
                continue;
            };
            for place in &o.places {
                let mut any = false;
                for file in below(tree, place) {
                    any = true;
                    reads.insert(file.to_string());
                }
                // A source the project names that the check's scope doesn't hold: what it is
                // now can't be known.
                if !any && of_kind(place) {
                    not.insert(not_followed("outside-scope", place.clone()));
                }
            }
            not.extend(o.not_followed.iter().cloned());
            for d in &o.deps {
                if d != id
                    && let Some(dep_name) = objects.get(d).and_then(|x| x.str("name"))
                {
                    out.edges.insert((
                        unit.clone(),
                        format!("{xcodeproj}/{dep_name}"),
                        crate::units::db::EdgeKind::Import,
                    ));
                }
                queue.push(d.clone());
            }
        }
        let mine: BTreeSet<String> = own[id]
            .places
            .iter()
            .flat_map(|place| below(tree, place).map(str::to_string))
            .filter(|f| of_kind(f))
            .collect();
        out.named
            .extend(reads.iter().filter(|f| of_kind(f)).cloned());
        out.by_name.insert(name.clone(), unit.clone());
        out.tests.insert(
            unit,
            TestTarget {
                name,
                reads,
                own: mine.into_iter().collect(),
                not_followed: not,
            },
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree_of(paths: &[&str]) -> Tree {
        let manifest: String = paths
            .iter()
            .map(|p| format!("f {} {p}\0", "0".repeat(64)))
            .collect();
        Tree::from_manifest(format!("{}\n{manifest}", hushrun_core::CANONICALIZATION).as_bytes())
            .expect("a tree")
    }

    fn fixture() -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../testdata/xcodebuild/Lib/Lib.xcodeproj/project.pbxproj"),
        )
        .unwrap()
    }

    const FILES: &[&str] = &[
        "ios/Lib/Lib.swift",
        "ios/Lib/One.swift",
        "ios/LibTests/LibTests.swift",
        "ios/OtherTests/OtherTests.swift",
        "ios/Lib.xcodeproj/project.pbxproj",
        "ios/README.md",
        "Other/Thing.swift",
    ];

    // covers: C-111
    #[test]
    fn a_test_target_reads_its_folders_and_those_of_the_targets_it_depends_on() {
        let tree = tree_of(FILES);
        let g = graph_of("ios/Lib.xcodeproj", &fixture(), &tree, Path::new("/work")).unwrap();
        let t = &g.tests["ios/Lib.xcodeproj/LibTests"];
        assert_eq!(t.name, "LibTests");
        assert_eq!(t.own, vec!["ios/LibTests/LibTests.swift".to_string()]);
        assert!(t.reads.contains("ios/Lib/Lib.swift"));
        assert!(t.reads.contains("ios/Lib/One.swift"));
        assert!(!t.reads.contains("Other/Thing.swift"));
        assert!(t.not_followed.is_empty(), "{:?}", t.not_followed);
        assert_eq!(g.tests.len(), 2, "a framework is no test target");
        let other = &g.tests["ios/Lib.xcodeproj/OtherTests"];
        assert_eq!(
            other.reads.iter().collect::<Vec<_>>(),
            vec!["ios/OtherTests/OtherTests.swift"]
        );
        assert_eq!(g.by_name["LibTests"], "ios/Lib.xcodeproj/LibTests");
        assert!(g.named.contains("ios/Lib/Lib.swift"));
        assert!(!g.named.contains("Other/Thing.swift"));
        assert!(g.is_unit("ios/LibTests/LibTests.swift"));
        assert!(!g.is_unit("ios/Lib/Lib.swift"));
        assert!(g.edges.contains(&(
            "ios/Lib.xcodeproj/LibTests".to_string(),
            "ios/Lib.xcodeproj/Lib".to_string(),
            crate::units::db::EdgeKind::Import
        )));
    }

    const CLASSIC: &str = r#"// !$*UTF8*$!
{
	objectVersion = 56;
	objects = {
		P = {isa = PBXProject; mainGroup = G; targets = (APP, T, UI); projectDirPath = "";
			attributes = {TargetAttributes = {T = {TestTargetID = APP;}; UI = {TestTargetID = APP;};};};
			packageReferences = (LP, RP);};
		G = {isa = PBXGroup; children = (SRC, TESTS, LOC, ABS); sourceTree = "<group>";};
		SRC = {isa = PBXGroup; children = (A, M); path = App; sourceTree = "<group>";};
		A = {isa = PBXFileReference; path = App.swift; sourceTree = "<group>";};
		M = {isa = PBXFileReference; path = Model.swift; sourceTree = "<group>";};
		TESTS = {isa = PBXGroup; children = (TF); name = Tests; path = AppTests; sourceTree = "<group>";};
		TF = {isa = PBXFileReference; path = AppTests.swift; sourceTree = "<group>";};
		LOC = {isa = PBXVariantGroup; children = (EN); name = Localizable.strings; sourceTree = "<group>";};
		EN = {isa = PBXFileReference; path = en.lproj/Localizable.strings; sourceTree = "<group>";};
		ABS = {isa = PBXFileReference; path = /elsewhere/Shared.swift; sourceTree = "<absolute>";};
		BA = {isa = PBXBuildFile; fileRef = A;};
		BM = {isa = PBXBuildFile; fileRef = M;};
		BT = {isa = PBXBuildFile; fileRef = TF;};
		BL = {isa = PBXBuildFile; fileRef = LOC;};
		BX = {isa = PBXBuildFile; fileRef = ABS;};
		APPSRC = {isa = PBXSourcesBuildPhase; files = (BA, BM);};
		APPRES = {isa = PBXResourcesBuildPhase; files = (BL);};
		TSRC = {isa = PBXSourcesBuildPhase; files = (BT);};
		UISRC = {isa = PBXSourcesBuildPhase; files = (BX);};
		APP = {isa = PBXNativeTarget; name = App; buildPhases = (APPSRC, APPRES); productType = "com.apple.product-type.application";
			packageProductDependencies = (LPD, RPD);};
		T = {isa = PBXNativeTarget; name = AppTests; buildPhases = (TSRC); productType = "com.apple.product-type.bundle.unit-test";};
		UI = {isa = PBXNativeTarget; name = AppUITests; buildPhases = (UISRC); productType = "com.apple.product-type.bundle.ui-testing";};
		LP = {isa = XCLocalSwiftPackageReference; relativePath = Packages/Kit;};
		RP = {isa = XCRemoteSwiftPackageReference; repositoryURL = "https://example.com/x";};
		LPD = {isa = XCSwiftPackageProductDependency; package = LP; productName = Kit;};
		RPD = {isa = XCSwiftPackageProductDependency; package = RP; productName = X;};
	};
	rootObject = P;
}
"#;

    // covers: C-111
    #[test]
    fn a_hosted_test_target_reads_its_app_with_its_packages_and_what_cant_be_followed_says_so() {
        let tree = tree_of(&[
            "App/App.swift",
            "App/Model.swift",
            "AppTests/AppTests.swift",
            "en.lproj/Localizable.strings",
            "Packages/Kit/Package.swift",
            "Packages/Kit/Sources/Kit/Kit.swift",
            "App.xcodeproj/project.pbxproj",
        ]);
        let g = graph_of("App.xcodeproj", CLASSIC, &tree, Path::new("/work")).unwrap();
        let t = &g.tests["App.xcodeproj/AppTests"];
        assert_eq!(t.own, vec!["AppTests/AppTests.swift".to_string()]);
        for f in [
            "App/App.swift",
            "App/Model.swift",
            "en.lproj/Localizable.strings",
            "Packages/Kit/Sources/Kit/Kit.swift",
        ] {
            assert!(t.reads.contains(f), "{f}");
        }
        // The remote package has no Package.resolved in the scope.
        assert!(t.not_followed.iter().any(|n| n.why == "unpinned"));
        let ui = &g.tests["App.xcodeproj/AppUITests"];
        assert!(
            ui.not_followed
                .iter()
                .any(|n| n.why == "outside-scope" && n.detail == "/elsewhere/Shared.swift")
        );

        let mut pinned: Vec<&str> = tree.paths().collect();
        pinned.push("App.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved");
        let g = graph_of(
            "App.xcodeproj",
            CLASSIC,
            &tree_of(&pinned),
            Path::new("/work"),
        )
        .unwrap();
        assert!(g.tests["App.xcodeproj/AppTests"].not_followed.is_empty());
    }

    // covers: C-111
    #[test]
    fn a_run_script_phase_reads_the_project_folder_whole() {
        let text = CLASSIC.replace(
            "APPSRC = {isa = PBXSourcesBuildPhase; files = (BA, BM);};",
            "APPSRC = {isa = PBXSourcesBuildPhase; files = (BA, BM);};\n\t\tSH = {isa = PBXShellScriptBuildPhase; shellScript = \"swiftlint\";};",
        )
        .replace("buildPhases = (APPSRC, APPRES)", "buildPhases = (APPSRC, APPRES, SH)");
        let tree = tree_of(&[
            "App/App.swift",
            "Scripts/gen.swift",
            "AppTests/AppTests.swift",
        ]);
        let g = graph_of("App.xcodeproj", &text, &tree, Path::new("/work")).unwrap();
        assert!(
            g.tests["App.xcodeproj/AppTests"]
                .reads
                .contains("Scripts/gen.swift")
        );
    }

    // covers: C-111
    #[test]
    fn a_target_of_another_project_isnt_followed() {
        let text = CLASSIC
            .replace(
                "T = {isa = PBXNativeTarget; name = AppTests; buildPhases = (TSRC);",
                "T = {isa = PBXNativeTarget; name = AppTests; buildPhases = (TSRC); dependencies = (D);",
            )
            .replace(
                "LP = {",
                "D = {isa = PBXTargetDependency; name = Sub; targetProxy = X;};\n\t\tLP = {",
            );
        let g = graph_of(
            "App.xcodeproj",
            &text,
            &tree_of(&["AppTests/AppTests.swift"]),
            Path::new("/work"),
        )
        .unwrap();
        assert!(
            g.tests["App.xcodeproj/AppTests"]
                .not_followed
                .iter()
                .any(|n| n.why == "another-project" && n.detail == "Sub")
        );
    }

    // covers: C-111
    #[test]
    fn a_project_file_that_doesnt_read_is_an_error() {
        let tree = tree_of(&[]);
        assert!(graph_of("A.xcodeproj", "{ objects = ", &tree, Path::new("/w")).is_err());
        assert!(graph_of("A.xcodeproj", "{ objects = {}; }", &tree, Path::new("/w")).is_err());
    }
}
