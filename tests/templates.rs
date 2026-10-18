//! The bundled templates parse, render and fit within their max_length_mm
//! on 12mm tape with typical values.
use sr5900p::label::error::LabelError;
use sr5900p::label::template::Template;
use sr5900p::label::Label;
use sr5900p::label::LabelData;
use sr5900p::label::Renderer;
use sr5900p::Tape;

fn render(name: &str, kv: &[(&str, &str)]) -> Result<Label, LabelError> {
    let t = Template::load(format!("{}/templates/{name}", env!("CARGO_MANIFEST_DIR")))?;
    let data = kv.iter().fold(LabelData::new(), |d, (k, v)| d.text(*k, *v));
    Renderer::new(t)?.render(&data, Tape::W12)
}

#[test]
fn bundled_templates_render_on_12mm() {
    let cases: &[(&str, &[(&str, &str)])] = &[
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
    for (name, kv) in cases {
        let td = render(name, kv).unwrap_or_else(|e| panic!("{name}: {e:#}"));
        assert_eq!(td.bitmap().height, Tape::W12.width_px() as usize, "{name}");
    }
}

#[test]
fn bundled_template_rejects_too_long_text() {
    let long = "長い資産名".repeat(10);
    let err = render(
        "asset.toml",
        &[("name", &long), ("id", "IT-2026-0042"), ("dept", "開発部")],
    )
    .expect_err("should be rejected");
    assert!(err.to_string().contains("exceeds max_length_mm"), "{err}");
}
