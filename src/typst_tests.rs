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
    let plan = plan::build_plan(&doc, &ExecuteDefaults::default());
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
            },
        );
    }
    store::write_cells_json(&project, &doc, &plan, &st).unwrap();
    let out = Command::new("typst")
        .args(["compile", "--input", "loom=strict"])
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
