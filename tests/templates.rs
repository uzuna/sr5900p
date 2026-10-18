//! The bundled templates (one directory per tape width) parse, render for their own tape
//! within their max_length_mm with typical values, and refuse other tapes.
use sr5900p::label::error::LabelError;
use sr5900p::label::template::Template;
use sr5900p::label::Label;
use sr5900p::label::LabelData;
use sr5900p::label::Renderer;
use sr5900p::Tape;

/// Bundled tape directories and the tape each one is for.
const TAPES: [(&str, Tape); 2] = [("12mm", Tape::W12), ("18mm", Tape::W18)];

fn renderer(dir: &str, name: &str) -> Renderer {
    let path = format!("{}/templates/{dir}/{name}", env!("CARGO_MANIFEST_DIR"));
    Renderer::new(Template::load(&path).unwrap()).unwrap_or_else(|e| panic!("{path}: {e:#}"))
}

fn render(r: &Renderer, kv: &[(&str, &str)], tape: Tape) -> Result<Label, LabelError> {
    let data = kv.iter().fold(LabelData::new(), |d, (k, v)| d.text(*k, *v));
    r.render(&data, tape)
}

const CASES: &[(&str, &[(&str, &str)])] = &[
    (
        "asset.toml",
        &[
            ("name", "ノートPC ThinkPad X1"),
            ("id", "IT-2026-0042"),
            ("dept", "開発部 第2グループ"),
        ],
    ),
    ("shelf.toml", &[("dept", "開発部"), ("shelf_id", "SH-A03")]),
    (
        "slot.toml",
        &[("slot_id", "SH-A03-05"), ("kinds", "ノートPC/タブレット")],
    ),
    (
        "card.toml",
        &[
            ("card_id", "EMP-0123"),
            ("token", "Qm9vR3x7kLp2ZsT1aWnY8cD4eF5gH6iJ7kL8mN9oP0q"),
        ],
    ),
];

#[test]
fn bundled_templates_render_on_their_tape() {
    for (dir, tape) in TAPES {
        for (name, kv) in CASES {
            let r = renderer(dir, name);
            assert_eq!(r.tape(), Some(tape), "{dir}/{name}");
            let label = render(&r, kv, tape).unwrap_or_else(|e| panic!("{dir}/{name}: {e:#}"));
            assert_eq!(
                label.bitmap().height,
                tape.width_px() as usize,
                "{dir}/{name}"
            );
        }
    }
}

#[test]
fn bundled_templates_refuse_another_tape() {
    for (dir, tape) in TAPES {
        let other = if tape == Tape::W12 {
            Tape::W18
        } else {
            Tape::W12
        };
        for (name, kv) in CASES {
            let err = render(&renderer(dir, name), kv, other).expect_err("other tape");
            assert!(
                matches!(err, LabelError::TemplateTapeMismatch { template, requested }
                    if template == tape && requested == other),
                "{dir}/{name}: {err:?}"
            );
        }
    }
}

fn entries(dir: &std::path::Path) -> Vec<(String, bool)> {
    let mut v: Vec<(String, bool)> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap())
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                e.path().is_dir(),
            )
        })
        .collect();
    v.sort();
    v
}

#[test]
fn every_bundled_template_names_the_tape_of_its_directory() {
    // a template without `tape`, or one dropped outside a tape directory, would be scaled
    // silently to whatever tape is loaded
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
    let dirs = entries(&root);
    assert!(
        dirs.iter().all(|(_, is_dir)| *is_dir),
        "only tape directories: {dirs:?}"
    );
    let names: Vec<&str> = dirs.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        TAPES.map(|(dir, _)| dir),
        "a new tape directory goes into TAPES"
    );
    let mut expected: Vec<&str> = CASES.iter().map(|(name, _)| *name).collect();
    expected.sort();
    for (dir, tape) in TAPES {
        let files = entries(&root.join(dir));
        let files: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        // every template is also rendered by the tests above
        assert_eq!(files, expected, "{dir}");
        for name in files {
            assert_eq!(renderer(dir, name).tape(), Some(tape), "{dir}/{name}");
        }
    }
}

#[test]
fn bundled_template_rejects_too_long_text() {
    let long = "長い資産名".repeat(10);
    let kv = [
        ("name", long.as_str()),
        ("id", "IT-2026-0042"),
        ("dept", "開発部"),
    ];
    for (dir, tape) in TAPES {
        let err = render(&renderer(dir, "asset.toml"), &kv, tape).expect_err("should be rejected");
        assert!(
            err.to_string().contains("exceeds max_length_mm"),
            "{dir}: {err}"
        );
    }
}

#[test]
fn template_naming_an_unknown_tape_is_rejected() {
    let t = Template::from_toml_str("tape = 13\n[[columns]]\ntype = \"qr\"\ndata = \"x\"\n", ".")
        .unwrap();
    let err = Renderer::new(t).err().expect("unknown tape");
    assert!(matches!(err, LabelError::InvalidTemplate(_)), "{err:?}");
}
