// SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: MIT

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

const BUILT_IN_COMMANDS: &[&str] = &[
    "new",
    "develop",
    "exit",
    "build",
    "sideload",
    "sim",
    "cert",
    "doctor",
    "preview",
    "logs",
    "plugin",
    "completions",
];

const SDK_AI_SKILLS: &[&str] = &["foundation-cli", "foundation-localize", "foundation-new-page"];

#[test]
fn foundation_cli_guide_covers_every_builtin_command() {
    let sdk = sdk_root();
    let guide = fs::read_to_string(sdk.join("docs").join("foundation-cli.md")).unwrap();
    let spec = fs::read_to_string(sdk.join("crates").join("cli").join("SPEC.md")).unwrap();
    let documented = documented_top_level_commands(&guide);

    for command in BUILT_IN_COMMANDS {
        assert!(
            documented.contains(*command),
            "`foundation {command}` is missing from sdk/docs/foundation-cli.md"
        );
        assert!(
            spec.contains(&format!("### `{command}`")),
            "`{command}` is missing a command section in sdk/crates/cli/SPEC.md"
        );
    }
}

#[test]
fn foundation_cli_skill_frontmatter_is_valid_for_codex_and_claude() {
    let sdk = sdk_root();

    for skill in SDK_AI_SKILLS {
        let codex_skill = sdk.join(".agents").join("skills").join(skill).join("SKILL.md");
        let claude_skill = sdk.join(".claude").join("skills").join(skill).join("SKILL.md");

        assert_eq!(
            fs::read_to_string(&codex_skill).unwrap(),
            fs::read_to_string(&claude_skill).unwrap(),
            "Codex and Claude should share the same {skill} skill content"
        );

        for path in [codex_skill, claude_skill] {
            let content = fs::read_to_string(&path).unwrap();
            assert_frontmatter_field(&path, &content, "name", skill);
            assert_frontmatter_field(&path, &content, "description", "Foundation");
            assert!(
                content.contains("disable-model-invocation: true")
                    || content.contains("docs/foundation-cli.md"),
                "{} should either be manually invocable or point at the canonical CLI guide",
                path.display()
            );
        }
    }
}

#[test]
fn foundation_cli_skill_mentions_every_builtin_command() {
    let sdk = sdk_root();

    for path in [
        sdk.join(".agents").join("skills").join("foundation-cli").join("SKILL.md"),
        sdk.join(".claude").join("skills").join("foundation-cli").join("SKILL.md"),
    ] {
        let content = fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("docs/foundation-cli.md")
                && content.contains("docs/guide/src/foundation-cli.md"),
            "{} should point agents at the canonical CLI guide",
            path.display()
        );
        for command in BUILT_IN_COMMANDS {
            assert!(
                content.contains(&format!("foundation {command}")),
                "{} should mention `foundation {command}` in its command quick reference",
                path.display()
            );
        }
    }
}

#[test]
fn workflow_skills_are_documented_in_cli_guide() {
    let sdk = sdk_root();
    let guide = fs::read_to_string(sdk.join("docs").join("foundation-cli.md")).unwrap();

    for skill in ["foundation-localize", "foundation-new-page"] {
        assert!(guide.contains(skill), "`/{skill}` is missing from sdk/docs/foundation-cli.md");
    }
}

#[test]
fn new_page_skill_does_not_stamp_foundation_headers_on_user_code() {
    let sdk = sdk_root();

    for path in [
        sdk.join(".agents").join("skills").join("foundation-new-page").join("SKILL.md"),
        sdk.join(".claude").join("skills").join("foundation-new-page").join("SKILL.md"),
    ] {
        let content = fs::read_to_string(&path).unwrap();
        let user_code_section = content.split("Create `ui/pages/<slug>/props.slint`").nth(1).unwrap_or("");
        assert!(
            !user_code_section.contains("SPDX-FileCopyrightText: 2026 Foundation Devices")
                && !user_code_section.contains("SPDX-License-Identifier"),
            "{} should not instruct agents to add SPDX headers to generated SDK-user code",
            path.display()
        );
        assert!(
            content.contains("Do not add Foundation copyright"),
            "{} should explicitly avoid Foundation headers in generated app files",
            path.display()
        );
    }
}

#[test]
fn sdk_build_installs_ai_tooling_into_common_bundle() {
    let sdk = sdk_root();
    let build_toml = fs::read_to_string(sdk.join("sdk-build.toml")).unwrap();

    assert!(build_toml.contains("source = \".agents\""));
    assert!(build_toml.contains("dest = \".agents\""));
    assert!(build_toml.contains("source = \".claude\""));
    assert!(build_toml.contains("dest = \".claude\""));
}

#[test]
fn app_templates_include_agent_cli_guidance() {
    let sdk = sdk_root();

    for template in ["default-app", "multi-page-app", "kitchen-sink"] {
        let agents =
            sdk.join("crates").join("cli").join("templates").join(template).join("files").join("AGENTS.md");
        let content = fs::read_to_string(&agents).unwrap();

        assert!(
            content.contains("sdk/docs/foundation-cli.md") && content.contains("foundation doctor"),
            "{} should point generated apps at the SDK CLI guide",
            agents.display()
        );
    }
}

#[test]
fn standalone_user_examples_include_license_headers() {
    let sdk = sdk_root();

    for root in [sdk.join("templates").join("hello-world"), sdk.join("examples").join("hello-world")] {
        for path in collect_files(&root) {
            let content = String::from_utf8_lossy(&fs::read(&path).unwrap()).to_string();
            assert!(
                content.contains("SPDX-FileCopyrightText") && content.contains("SPDX-License-Identifier"),
                "{} should include SPDX metadata",
                path.display()
            );
        }
    }
}

fn documented_top_level_commands(guide: &str) -> BTreeSet<String> {
    guide
        .lines()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix("| `foundation ")?;
            let invocation = rest.split('`').next()?;
            invocation.split_whitespace().next().map(str::to_string)
        })
        .collect()
}

fn collect_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect_files_inner(root, &mut files);
    files
}

fn collect_files_inner(root: &Path, files: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        if path.is_dir() {
            collect_files_inner(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn assert_frontmatter_field(path: &Path, content: &str, key: &str, expected: &str) {
    let mut lines = content.lines();
    assert_eq!(lines.next(), Some("---"), "{} is missing YAML frontmatter", path.display());

    let prefix = format!("{key}:");
    let value = lines
        .by_ref()
        .take_while(|line| *line != "---")
        .find_map(|line| line.strip_prefix(&prefix).map(str::trim))
        .unwrap_or_else(|| panic!("{} is missing frontmatter field `{key}`", path.display()));

    assert!(
        value.contains(expected),
        "{} frontmatter field `{key}` should contain `{expected}`, got `{value}`",
        path.display()
    );
}

fn sdk_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().and_then(Path::parent).unwrap().to_path_buf()
}
