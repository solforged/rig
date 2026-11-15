use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn documents() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = vec![root.join("README.md")];
    files.extend(
        fs::read_dir(root.join("docs"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "md")),
    );
    files
}
fn anchors(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.starts_with('#'))
        .map(|line| {
            line.trim_start_matches('#')
                .trim()
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
                .map(|c| if c == ' ' { '-' } else { c })
                .collect()
        })
        .collect()
}
#[test]
fn documentation_links_and_fences_are_valid() {
    for path in documents() {
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.ends_with('\n'),
            "{} lacks a final newline",
            path.display()
        );
        assert_eq!(
            text.lines().filter(|line| line.starts_with("```")).count() % 2,
            0,
            "unbalanced fences in {}",
            path.display()
        );
        for part in text.split("](").skip(1) {
            let Some((target, _)) = part.split_once(')') else {
                continue;
            };
            if target.contains("://") || target.starts_with("../../system/") {
                continue;
            }
            let (file, anchor) = target
                .split_once('#')
                .map_or((target, None), |(p, a)| (p, Some(a)));
            let destination = if file.is_empty() {
                path.clone()
            } else {
                path.parent().unwrap().join(file)
            };
            assert!(destination.exists(), "{}: missing {target}", path.display());
            if let Some(anchor) = anchor {
                assert!(
                    anchors(&fs::read_to_string(&destination).unwrap())
                        .iter()
                        .any(|a| a == anchor),
                    "{}: missing {target}",
                    path.display()
                );
            }
        }
    }
}
#[test]
fn documented_shell_examples_parse_without_execution() {
    for path in documents() {
        let text = fs::read_to_string(&path).unwrap();
        for part in text.split("```sh\n").skip(1) {
            let code = part.split_once("```").unwrap().0;
            let mut child = Command::new("bash")
                .arg("-n")
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(code.as_bytes())
                .unwrap();
            assert!(
                child.wait().unwrap().success(),
                "invalid shell example in {}",
                path.display()
            );
        }
    }
}
