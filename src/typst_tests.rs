//! Tests that compile documents with the `typst` CLI against the Typst
//! library. They are skipped when `typst` is not on PATH.

use crate::config::{ExecuteDefaults, Project};
use crate::store::{Entry, Store};
use crate::{plan, store, syntax};
use std::path::Path;
use std::process::Command;

const LOOM_TYP: &str = include_str!("../typst/loom.typ");

fn typst_available() -> bool {
    Command::new("typst").arg("--version").output().is_ok()
}

/// Parse `files`, store an empty successful result for every cell, write
/// `cells.json`, and compile in strict mode. Strict mode fails for any cell
/// whose lookup key, as computed by the Typst library, differs from the one
/// computed by the parser.
fn compile_strict(files: &[(&str, &str)]) -> Result<String, String> {
    compile_edited(files, &[], true)
}

/// Like `compile_strict`, but after writing `cells.json` the files in
/// `edits` replace those of `files`, as when a source is changed in the
/// editor and not yet run.
fn compile_edited(files: &[(&str, &str)], edits: &[(&str, &str)], strict: bool) -> Result<String, String> {
    let dir = tempfile::tempdir().unwrap();
    for (name, content) in files {
        let p = dir.path().join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    let project = Project::from_root(&dir.path().join(files[0].0)).unwrap();
    std::fs::create_dir_all(project.loom_dir()).unwrap();
    std::fs::write(project.loom_dir().join("loom.typ"), LOOM_TYP).unwrap();
    let doc = syntax::parse_document(&project).map_err(|e| e.to_string())?;
    let plan = plan::build_plan(&doc, &ExecuteDefaults::default()).map_err(|e| e.to_string())?;
    let mut st = Store::default();
    for (i, key) in plan.cell_keys.iter().enumerate() {
        let Some(key) = key else { continue };
        st.insert(
            key.clone(),
            Entry {
                lang: doc.cells[i].lang.tag().into(),
                session: doc.cells[i].session().into(),
                failed: false,
                error_ok: false,
                outputs: vec![serde_json::json!({
                    "type": "value", "repl_only": false,
                    "data": {"text/plain": format!("result {i}")}
                })],
                exec: doc.cells[i].options.exec_raw(),
                fig_width: 7.0,
                fig_height: 5.0,
                used: 0,
                env: None,
                exports: Default::default(),
            },
        );
    }
    st.remember_labels(&doc, &plan);
    store::write_cells_json(&project, &doc, &plan, &st).unwrap();
    for (name, content) in edits {
        std::fs::write(dir.path().join(name), content).unwrap();
    }
    let mut cmd = Command::new("typst");
    cmd.arg("compile");
    if strict {
        cmd.args(["--input", "loom=strict"]);
    }
    let out = cmd
        .arg(&project.root_file)
        .arg(dir.path().join("out.pdf"))
        .output()
        .unwrap();
    if out.status.success() {
        Ok(text_of(&dir.path().join("out.pdf")))
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

fn text_of(pdf: &Path) -> String {
    Command::new("pdftotext")
        .arg(pdf)
        .arg("-")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

const PREAMBLE: &str = "#import \".loom/loom.typ\": loom\n#show: loom\n";

#[test]
fn lookup_keys_agree_with_typst() {
    if !typst_available() {
        eprintln!("typst not found; skipping");
        return;
    }
    let doc = format!(
        "{PREAMBLE}\
#figure(caption: [Indented.])[\n    ```julia\n    #| fig-width: 4\n    x = 1\n\n      y = 2   \n    ```\n]\n\
```r\n  #|  echo:   false  \nz <- \"#| not an option\"\n\tw <- 'tab'\n```\n\
```python\nπ = 3.14  # ünïcødé 🎉\n```\n\
Twice: `{{r}}   1 + 1  ` and `{{r}} 1 + 1`.\n\
```julia\nx\n```\n```julia\nx\n```\n\
```julia\n#| echo: false\n```\n\
#include \"crlf.typ\"\n"
    );
    let crlf = "```julia\r\na = 1\r\nb = 2\r\n```\r\n";
    match compile_strict(&[("doc.typ", &doc), ("crlf.typ", crlf)]) {
        Ok(_) => {}
        Err(e) => panic!("strict compile failed:\n{e}"),
    }
}

#[test]
fn strict_mode_rejects_unexecuted_cells() {
    if !typst_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("doc.typ");
    std::fs::write(&root, format!("{PREAMBLE}```julia\nnot_run()\n```\n")).unwrap();
    std::fs::create_dir_all(dir.path().join(".loom")).unwrap();
    std::fs::write(dir.path().join(".loom/loom.typ"), LOOM_TYP).unwrap();
    std::fs::write(dir.path().join(".loom/cells.json"), r#"{"version":1,"cells":{}}"#).unwrap();
    let strict = Command::new("typst")
        .args(["compile", "--input", "loom=strict"])
        .arg(&root)
        .arg(dir.path().join("out.pdf"))
        .output()
        .unwrap();
    assert!(!strict.status.success());
    assert!(String::from_utf8_lossy(&strict.stderr).contains("not_run()` is pending"));
    let draft = Command::new("typst")
        .arg("compile")
        .arg(&root)
        .arg(dir.path().join("out.pdf"))
        .output()
        .unwrap();
    assert!(draft.status.success(), "{}", String::from_utf8_lossy(&draft.stderr));
}

#[test]
fn repeated_cells_get_results_in_document_order() {
    if !typst_available() {
        return;
    }
    let text = compile_strict(&[(
        "doc.typ",
        &format!("{PREAMBLE}```julia\nx\n```\n```julia\ny\n```\n```julia\nx\n```\n"),
    )])
    .unwrap();
    let order: Vec<&str> = text.lines().filter(|l| l.starts_with("result")).collect();
    assert_eq!(order, ["result 0", "result 1", "result 2"]);
}

/// Text of each cell's status banner (or "ok") and result, in order.
fn statuses(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.starts_with("loom · not executed") {
                Some("pending".to_string())
            } else if l.starts_with("loom · stale") {
                Some("stale".to_string())
            } else if l.starts_with("result") {
                Some(l.to_string())
            } else {
                None
            }
        })
        .collect()
}

#[test]
fn edits_propagate_within_a_session_only() {
    if !typst_available() {
        return;
    }
    let cells = |b: &str| {
        format!(
            "{PREAMBLE}```julia\na = 1\n```\n```julia\n{b}\n```\n```julia\nc = 3\n```\n\
```julia\n#| session: other\nd = 4\n```\n"
        )
    };
    let original = cells("b = 2");
    // Editing the second cell: it has no result; the third is stale; the
    // cell of the other session is untouched.
    let text = compile_edited(&[("doc.typ", &original)], &[("doc.typ", &cells("b = 20"))], false).unwrap();
    assert_eq!(
        statuses(&text),
        ["result 0", "pending", "stale", "result 2", "result 3"]
    );
    // Deleting the second cell makes the third stale.
    let deleted = format!(
        "{PREAMBLE}```julia\na = 1\n```\n```julia\nc = 3\n```\n```julia\n#| session: other\nd = 4\n```\n"
    );
    let text = compile_edited(&[("doc.typ", &original)], &[("doc.typ", &deleted)], false).unwrap();
    assert_eq!(statuses(&text), ["result 0", "stale", "result 2", "result 3"]);
    // Strict mode rejects the stale cell.
    assert!(compile_edited(&[("doc.typ", &original)], &[("doc.typ", &deleted)], true).is_err());
}

#[test]
fn editing_a_stateless_cell_leaves_later_cells_current() {
    if !typst_available() {
        return;
    }
    let cells = |b: &str| {
        format!(
            "{PREAMBLE}```julia\na = 1\n```\n```julia\n#| stateless: true\n{b}\n```\n```julia\nc = 3\n```\n"
        )
    };
    let text = compile_edited(&[("doc.typ", &cells("show(a)"))], &[("doc.typ", &cells("show(a + 1)"))], false).unwrap();
    assert_eq!(statuses(&text), ["result 0", "pending", "result 2"]);
}

#[test]
fn an_edited_labeled_cell_keeps_its_last_result() {
    if !typst_available() {
        return;
    }
    let cells = |b: &str| {
        format!("{PREAMBLE}```julia\n#| label: fig-a\n#| fig-cap: A.\n{b}\n```\n")
    };
    let text = compile_edited(&[("doc.typ", &cells("plot(1)"))], &[("doc.typ", &cells("plot(2)"))], false).unwrap();
    assert_eq!(statuses(&text), ["stale", "result 0"]);
}

#[test]
fn editing_an_exporter_makes_its_importers_stale() {
    if !typst_available() {
        return;
    }
    let doc = |r: &str| {
        format!(
            "{PREAMBLE}```r\n#| export: df\n{r}\n```\n```python\nx = 1\n```\n```python\n#| import: df\ndf.shape\n```\n"
        )
    };
    let text = compile_edited(&[("doc.typ", &doc("df <- mtcars"))], &[("doc.typ", &doc("df <- iris"))], false).unwrap();
    assert_eq!(statuses(&text), ["pending", "result 1", "stale", "result 2"]);
}

/// Compile a document whose every cell produced `images` SVG images.
fn compile_with_images(doc: &str, images: usize, strict: bool) -> Result<String, String> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("doc.typ");
    std::fs::write(&root, doc).unwrap();
    let project = Project::from_root(&root).unwrap();
    std::fs::create_dir_all(project.figures_dir()).unwrap();
    std::fs::write(project.loom_dir().join("loom.typ"), LOOM_TYP).unwrap();
    std::fs::write(
        project.figures_dir().join("box.svg"),
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="60"><rect width="100" height="60" fill="gray"/></svg>"#,
    )
    .unwrap();
    let parsed = syntax::parse_document(&project).map_err(|e| e.to_string())?;
    let plan = plan::build_plan(&parsed, &ExecuteDefaults::default()).map_err(|e| e.to_string())?;
    let mut st = Store::default();
    for (i, key) in plan.cell_keys.iter().enumerate() {
        let Some(key) = key else { continue };
        let outputs = (0..images)
            .map(|_| serde_json::json!({"type": "display", "data": {"image/svg+xml": {"file": "figures/box.svg"}}}))
            .collect();
        st.insert(
            key.clone(),
            Entry {
                lang: parsed.cells[i].lang.tag().into(),
                session: parsed.cells[i].session().into(),
                failed: false,
                error_ok: false,
                outputs,
                exec: parsed.cells[i].options.exec_raw(),
                fig_width: 2.0,
                fig_height: 1.0,
                used: 0,
                env: None,
                exports: Default::default(),
            },
        );
    }
    store::write_cells_json(&project, &parsed, &plan, &st).unwrap();
    let mut cmd = Command::new("typst");
    cmd.arg("compile");
    if strict {
        cmd.args(["--input", "loom=strict"]);
    }
    let out = cmd.arg(&root).arg(dir.path().join("out.pdf")).output().unwrap();
    if out.status.success() {
        Ok(text_of(&dir.path().join("out.pdf")))
    } else {
        Err(String::from_utf8_lossy(&out.stderr).to_string())
    }
}

#[test]
fn subfigures_are_lettered_and_referenced() {
    if !typst_available() {
        return;
    }
    let doc = format!(
        "{PREAMBLE}#set heading(numbering: \"1.\")\n#set figure(numbering: n => numbering(\"1.1\", counter(heading).get().first(), n))\n\
= One\n#figure(rect(), caption: [Static.]) <fig-static>\n\
```julia\n#| label: fig-panel\n#| fig-cap: Main.\n#| fig-subcap:\n#|   - \"First, with a comma\"\n#|   - Second\n#| layout-ncol: 2\nplots()\n```\n\
```python\n#| fig-cap: Unlabeled.\n#| fig-subcap: ['Left', \"Right\"]\nplots()\n```\n\
Refs: @fig-panel, @fig-panel-1, @fig-panel-2, @fig-static.\n#outline(target: figure.where(kind: image))\n"
    );
    let text = compile_with_images(&doc, 2, true).unwrap();
    for expected in [
        "(a) First, with a comma",
        "(b) Second",
        "Figure 1.2: Main.",
        "(a) Left",
        "(b) Right",
        "Figure 1.3: Unlabeled.",
        "Refs: Figure 1.2, Figure 1.2a, Figure 1.2b, Figure 1.1.",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
    // Only parent figures are listed in the outline.
    assert!(!text.contains("Figure 1.2a First"), "{text}");
}

#[test]
fn a_subcaption_count_mismatch_fails_in_strict_mode() {
    if !typst_available() {
        return;
    }
    let doc = format!("{PREAMBLE}```julia\n#| fig-subcap: [\"Only one\"]\nplots()\n```\n");
    let err = compile_with_images(&doc, 2, true).unwrap_err();
    assert!(err.contains("has 1 subcaptions for 2 images"), "{err}");
    let draft = compile_with_images(&doc, 2, false).unwrap();
    assert!(draft.contains("1 subcaptions for 2 images"), "{draft}");
}
